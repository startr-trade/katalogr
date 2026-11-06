//! The view routes of the HTTP/JSON API (FEAT-088): `GET /api/v1/views/` and
//! `GET /api/v1/views/<op>`.

#[path = "views_support/mod.rs"]
mod support;

mod views {
    use super::support::*;
    use serde_json::Value;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    pub struct Server {
        child: Child,
        pub addr: SocketAddr,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    pub fn serve(repo: &Path) -> Server {
        let log = repo.join("../api.log");
        let _ = std::fs::remove_file(&log);
        let mut child = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(repo)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(["api", "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .expect("spawn katalogr api");
        let started = Instant::now();
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if let Some(i) = text.find("listening on http://") {
                let rest = &text[i + "listening on http://".len()..];
                let addr = rest.lines().next().unwrap().trim().parse().unwrap();
                return Server { child, addr };
            }
            if let Ok(Some(st)) = child.try_wait() {
                panic!("katalogr api exited early ({st}): {text}");
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "no start: {text}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[derive(Debug)]
    pub struct Reply {
        pub status: u16,
        pub headers: Vec<(String, String)>,
        pub body: String,
    }

    impl Reply {
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }

        pub fn json(&self) -> Value {
            serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {self:?}"))
        }
    }

    pub fn request(s: &Server, path: &str, extra: &[(&str, &str)]) -> Reply {
        let mut req = format!("GET {path} HTTP/1.0\r\nHost: {}\r\n", s.addr);
        for (k, v) in extra {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("Connection: close\r\n\r\n");
        let mut c = TcpStream::connect(s.addr).expect("connect");
        c.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        c.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        let _ = c.read_to_end(&mut buf);
        let text = String::from_utf8_lossy(&buf).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        Reply {
            status,
            headers,
            body: body.to_string(),
        }
    }

    pub fn get(s: &Server, path: &str) -> Reply {
        request(s, path, &[])
    }

    /// R-1. The route answers Mermaid and the graph, with the view ID as its `ETag`.
    #[test]
    fn api_returns_mermaid_and_graph() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo);
        let r = get(
            &server,
            "/api/v1/views/structure?level=file&consistency=wait",
        );
        assert_eq!(r.status, 200, "{r:?}");
        let v = r.json();
        let schema = view_schema();
        validate(&schema, &schema, &v, "$").unwrap_or_else(|e| panic!("{e}\n{}", r.body));
        assert_eq!(v["exact"], true);
        assert!(v["mermaid"].as_str().unwrap().starts_with("flowchart"));
        assert!(
            v["graph"]["nodes"]
                .as_array()
                .is_some_and(|n| !n.is_empty())
        );
        let id = v["view_id"].as_str().unwrap();
        assert_eq!(r.header("etag"), Some(format!("\"{id}\"").as_str()));
        assert_eq!(r.header("x-katalogr-view"), Some("miss"));
        // The other ops take their positional arguments by name.
        for q in [
            "contracts",
            "callers?symbol=target&depth=2",
            "path?from=top&to=caller_a",
            "cycles?level=file",
        ] {
            let sep = if q.contains('?') { '&' } else { '?' };
            let r = get(&server, &format!("/api/v1/views/{q}{sep}consistency=wait"));
            assert_eq!(r.status, 200, "{q}: {r:?}");
            validate(&schema, &schema, &r.json(), "$").unwrap_or_else(|e| panic!("{q}: {e}"));
        }
    }

    /// R-1. The API body is the CLI's `--json`, byte for byte.
    #[test]
    fn api_body_equals_cli_json() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo);
        let r = get(
            &server,
            "/api/v1/views/structure?level=file&consistency=wait",
        );
        assert_eq!(r.status, 200, "{r:?}");
        let cli_out = ok(&repo, &["--json", "view", "structure", "--level", "file"]);
        assert_eq!(r.body, cli_out.trim_end());
        // The second request is a hit.
        let again = get(
            &server,
            "/api/v1/views/structure?level=file&consistency=wait",
        );
        assert_eq!(again.body, r.body);
        assert_eq!(again.header("x-katalogr-view"), Some("hit"));
    }

    /// R-6. `meta` lists the capability `views`.
    #[test]
    fn capability_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo);
        let v = get(&server, "/api/v1/meta").json();
        let caps: Vec<&str> = v["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(caps.contains(&"views"), "{caps:?}");
    }

    /// R-6. The catalogue names every op with its arguments, sorted, and has the shape the
    /// shared file records.
    #[test]
    fn catalogue_lists_ops() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo);
        let r = get(&server, "/api/v1/views/");
        assert_eq!(r.status, 200, "{r:?}");
        let v = r.json();
        assert_eq!(v["schema"], 1);
        assert_eq!(v["pele"], "1.7.0");
        let ops: Vec<&str> = v["views"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["op"].as_str().unwrap())
            .collect();
        assert_eq!(ops, ["callers", "contracts", "cycles", "path", "structure"]);
        let callers = &v["views"][0];
        let names: Vec<&str> = callers["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["symbol", "depth", "max-nodes"]);
        assert_eq!(callers["params"][0]["required"], true);
        assert_eq!(callers["params"][1]["default"], "2");
        // The shared file records this shape.
        let shared: Value = serde_json::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/contract/views-v1.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(v, shared["catalogue"]);
    }

    /// R-6. The contract number is raised for the new capability.
    #[test]
    fn contract_raised() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo);
        let v = get(&server, "/api/v1/meta").json();
        assert_eq!(v["contract"], 4);
        assert_eq!(v["view_format"], 1);
    }

    /// R-7. An op that is not a view is 404.
    #[test]
    fn unknown_op_404() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo);
        for p in [
            "/api/v1/views/graph",
            "/api/v1/views",
            "/api/v1/views/structure/x",
        ] {
            let r = get(&server, p);
            assert_eq!(r.status, 404, "{p}: {r:?}");
            assert_eq!(r.json()["error"], "not-found");
        }
    }

    /// R-7. An unknown or misplaced parameter, or a bad value, is 400.
    #[test]
    fn unknown_param_400() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo);
        for q in [
            "structure?nope=1",
            "structure?level=symbol",
            "structure?max-nodes=0",
            "structure?budget=100",
            "callers",
            "path?from=a",
        ] {
            let r = get(&server, &format!("/api/v1/views/{q}"));
            assert_eq!(r.status, 400, "{q}: {r:?}");
            assert_eq!(r.json()["error"], "usage");
        }
        assert_eq!(get(&server, "/api/v1/views/?x=1").status, 400);
    }

    /// R-7. A position no context holds is 409, naming the ones that are held.
    #[test]
    fn position_unavailable_409() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo);
        let none = "0".repeat(40);
        let r = get(
            &server,
            &format!("/api/v1/views/structure?at={none}&consistency=wait"),
        );
        assert_eq!(r.status, 409, "{r:?}");
        let v = r.json();
        assert!(
            v.get("available").is_some() || v.get("message").is_some(),
            "{v}"
        );
    }

    /// R-4. An answer that is not exact carries no `ETag` and is marked.
    #[test]
    fn not_exact_has_no_etag() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo);
        // The default consistency of the API is `stale-ok`, which never reconciles.
        let r = get(&server, "/api/v1/views/structure?level=file");
        assert_eq!(r.status, 200, "{r:?}");
        assert_eq!(r.json()["exact"], false);
        assert_eq!(r.header("etag"), None);
        assert!(r.json().get("view_id").is_none());
        assert_eq!(r.header("x-katalogr-view"), Some("miss"));
    }

    /// R-1. `If-None-Match` with the current ID is 304 without a body.
    #[test]
    fn not_modified_by_view_id() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo);
        let path = "/api/v1/views/structure?level=file&consistency=wait";
        let first = get(&server, path);
        let etag = first.header("etag").expect("etag").to_string();
        let same = request(&server, path, &[("If-None-Match", &etag)]);
        assert_eq!(same.status, 304, "{same:?}");
        assert_eq!(same.body, "");
        assert_eq!(same.header("etag"), Some(etag.as_str()));
        let other = request(&server, path, &[("If-None-Match", "\"nope\"")]);
        assert_eq!(other.status, 200);
    }
}
