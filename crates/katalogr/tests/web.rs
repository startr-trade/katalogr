#![cfg(feature = "web")]
//! FEAT-033: `katalogr serve`, the API plus the embedded pages, against a synthetic `.tx`
//! project. No board exists anywhere.

#[path = "views_support/mod.rs"]
mod support;

mod web {
    use super::support::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    struct Server {
        child: Child,
        addr: SocketAddr,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn start(repo: &Path, command: &str) -> Server {
        let log = repo.join(format!("../{command}.log"));
        let _ = std::fs::remove_file(&log);
        let mut child = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(repo)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env_remove("KANBANR_DATA_DIR")
            .args([command, "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .expect("spawn katalogr");
        let started = Instant::now();
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if let Some(i) = text.find("listening on http://") {
                let rest = &text[i + "listening on http://".len()..];
                assert!(
                    text.contains(&format!("katalogr {command}: listening")),
                    "{text}"
                );
                let addr = rest.lines().next().unwrap().trim().parse().unwrap();
                return Server { child, addr };
            }
            if let Ok(Some(st)) = child.try_wait() {
                panic!("katalogr {command} exited early ({st}): {text}");
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "no start: {text}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[derive(Debug)]
    struct Reply {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Reply {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    fn request(s: &Server, method: &str, path: &str, extra: &[(&str, &str)]) -> Reply {
        let mut req = format!("{method} {path} HTTP/1.0\r\n");
        if !extra.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            req.push_str(&format!("Host: {}\r\n", s.addr));
        }
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

    fn get(s: &Server, path: &str) -> Reply {
        request(s, "GET", path, &[])
    }

    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        (tmp, repo)
    }

    #[test]
    fn serve_answers_pages_and_api() {
        let (_t, repo) = setup();
        let s = start(&repo, "serve");
        let page = get(&s, "/");
        assert_eq!(page.status, 200);
        assert!(
            page.header("content-type")
                .unwrap()
                .starts_with("text/html")
        );
        assert_eq!(page.body, katalogr_web::page_linked(""));
        assert!(
            page.body
                .contains("<meta name=\"katalogr-api\" content=\"\">")
        );
        for a in katalogr_web::assets() {
            let r = get(&s, &format!("/assets/{}", a.name));
            assert_eq!(r.status, 200, "{}", a.name);
            assert_eq!(r.body, a.text, "{}", a.name);
            assert_eq!(r.header("content-type"), Some(a.mime), "{}", a.name);
        }
        assert_eq!(get(&s, "/api/v1/meta").status, 200);
        assert_eq!(get(&s, "/nope").status, 404);
        assert_eq!(get(&s, "/assets/..%2Fx").status, 404);
        assert_eq!(get(&s, "/assets/nope.js").status, 404);
    }

    #[test]
    fn api_body_same_under_serve() {
        let (_t, repo) = setup();
        let api = start(&repo, "api");
        let serve = start(&repo, "serve");
        for p in [
            "/api/v1/find?text=target",
            "/api/v1/status",
            "/api/v1/views/",
            "/api/v1/views/structure",
            "/api/v1/timeline",
            "/api/v1/openapi.json",
        ] {
            let (a, b) = (get(&api, p), get(&serve, p));
            assert_eq!((a.status, &a.body), (b.status, &b.body), "{p}");
        }
    }

    #[test]
    fn only_get_on_pages() {
        let (_t, repo) = setup();
        let s = start(&repo, "serve");
        for m in ["POST", "PUT", "DELETE", "PATCH", "HEAD"] {
            assert_eq!(request(&s, m, "/", &[]).status, 405, "{m}");
            assert_eq!(request(&s, m, "/assets/kw-core.js", &[]).status, 405, "{m}");
        }
    }

    #[test]
    fn every_route_refuses_writes() {
        let (_t, repo) = setup();
        let s = start(&repo, "serve");
        for p in [
            "/",
            "/assets/kw-main.js",
            "/api/v1/meta",
            "/api/v1/find?text=x",
            "/api/v1/timeline",
            "/api/v1/stored-view?id=v1-00000000000000000000000000000000",
            "/anything",
        ] {
            for m in ["POST", "PUT", "DELETE"] {
                assert_eq!(request(&s, m, p, &[]).status, 405, "{m} {p}");
            }
        }
    }

    #[test]
    fn api_serves_no_pages() {
        let (_t, repo) = setup();
        let s = start(&repo, "api");
        assert_eq!(get(&s, "/").status, 404);
        assert_eq!(get(&s, "/assets/kw-core.js").status, 404);
    }

    #[test]
    fn capability_only_under_serve() {
        let (_t, repo) = setup();
        let caps = |s: &Server| -> Vec<String> {
            let v: serde_json::Value = serde_json::from_str(&get(s, "/api/v1/meta").body).unwrap();
            v["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_str().unwrap().to_string())
                .collect()
        };
        let api = start(&repo, "api");
        assert!(!caps(&api).contains(&"web-view".to_string()));
        let serve = start(&repo, "serve");
        let c = caps(&serve);
        assert!(c.contains(&"web-view".to_string()), "{c:?}");
        let mut sorted = c.clone();
        sorted.sort();
        assert_eq!(c, sorted);
        // The contract command describes the binary, not a server.
        assert!(!ok(&repo, &["--json", "contract"]).contains("web-view"));
    }

    #[test]
    fn runs_without_board() {
        let (_t, repo) = setup();
        let s = start(&repo, "serve");
        assert_eq!(get(&s, "/").status, 200);
        assert_eq!(get(&s, "/api/v1/views/structure").status, 200);
        assert_eq!(get(&s, "/api/v1/hubs").status, 200);
    }

    #[test]
    fn no_matrix_page() {
        let (_t, repo) = setup();
        let s = start(&repo, "serve");
        assert!(!get(&s, "/").body.to_lowercase().contains("matrix"));
        assert!(
            !get(&s, "/assets/views.json")
                .body
                .to_lowercase()
                .contains("matrix")
        );
        assert_eq!(get(&s, "/matrix").status, 404);
        assert_eq!(get(&s, "/assets/matrix.js").status, 404);
    }

    #[test]
    fn pages_behind_host_check() {
        let (_t, repo) = setup();
        let s = start(&repo, "serve");
        for p in ["/", "/assets/kw-core.js"] {
            let r = request(&s, "GET", p, &[("Host", "evil.example:80")]);
            assert_eq!(r.status, 403, "{p}");
            assert!(!r.body.contains("<html"));
        }
        let r = request(
            &s,
            "GET",
            "/",
            &[("Host", &format!("localhost:{}", s.addr.port()))],
        );
        assert_eq!(r.status, 200);
    }

    #[test]
    fn non_loopback_refused() {
        let (_t, repo) = setup();
        let out = cli(&repo, &["serve", "--listen", "0.0.0.0:0"]);
        assert!(!out.status.success(), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .to_lowercase()
                .contains("loopback")
        );
    }

    #[test]
    fn page_security_headers() {
        let (_t, repo) = setup();
        let s = start(&repo, "serve");
        let r = get(&s, "/");
        let csp = r.header("content-security-policy").unwrap();
        for want in [
            "default-src 'none'",
            "script-src 'self'",
            "connect-src 'self'",
            "base-uri 'none'",
            "form-action 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(csp.contains(want), "{csp}");
        }
        assert!(!csp.contains("unsafe-eval"));
        assert!(
            !csp.split(';')
                .any(|d| d.contains("script-src") && d.contains("unsafe-inline"))
        );
        assert_eq!(r.header("x-frame-options"), Some("DENY"));
        assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(r.header("referrer-policy"), Some("no-referrer"));
        assert_eq!(r.header("cache-control"), Some("no-cache"));
        let etag = r.header("etag").unwrap().to_string();
        let again = request(&s, "GET", "/", &[("If-None-Match", &etag)]);
        assert_eq!(again.status, 304);
        assert!(again.body.is_empty());
        let a = get(&s, "/assets/kw-core.js");
        assert_eq!(a.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(a.header("content-type"), Some("text/javascript"));
        assert!(a.header("content-security-policy").is_none());
        let a2 = request(
            &s,
            "GET",
            "/assets/kw-core.js",
            &[("If-None-Match", a.header("etag").unwrap())],
        );
        assert_eq!(a2.status, 304);
    }

    // ---- R-19 ----

    #[test]
    fn write_assets_equal_served_bytes() {
        let (_t, repo) = setup();
        let s = start(&repo, "serve");
        let out = tempfile::tempdir().unwrap();
        let dir = out.path().join("site");
        ok(&repo, &["serve", "--write-assets", dir.to_str().unwrap()]);
        assert_eq!(
            std::fs::read_to_string(dir.join("index.html")).unwrap(),
            get(&s, "/").body
        );
        for a in katalogr_web::assets() {
            assert_eq!(
                std::fs::read_to_string(dir.join("assets").join(a.name)).unwrap(),
                get(&s, &format!("/assets/{}", a.name)).body,
                "{}",
                a.name
            );
        }
        // With an API address, only the meta tag differs.
        let other = out.path().join("remote");
        ok(
            &repo,
            &[
                "serve",
                "--write-assets",
                other.to_str().unwrap(),
                "--api-base",
                "http://127.0.0.1:7878",
            ],
        );
        let page = std::fs::read_to_string(other.join("index.html")).unwrap();
        assert!(page.contains("content=\"http://127.0.0.1:7878\""));
    }

    #[test]
    fn write_assets_needs_no_project() {
        let empty = tempfile::tempdir().unwrap();
        let dir = empty.path().join("site");
        let out = cli(
            empty.path(),
            &["serve", "--write-assets", dir.to_str().unwrap()],
        );
        assert!(out.status.success(), "{out:?}");
        assert!(dir.join("index.html").is_file());
        assert!(dir.join("assets/kw-core.js").is_file());
    }

    #[test]
    fn api_base_needs_write_assets() {
        let empty = tempfile::tempdir().unwrap();
        let out = cli(empty.path(), &["serve", "--api-base", "http://x"]);
        assert_eq!(out.status.code(), Some(2), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("--write-assets"));
    }

    /// Capabilities a view may name before the item that serves them is built: their pages or
    /// panels read as absent on purpose. Every other `needs` must be a capability this binary
    /// reports, so a renamed capability cannot silently hide a page (FEAT-115).
    const PLANNED: &[&str] = &[
        "drift",
        "git-history",
        "metrics",
        "rules",
        "vulnerabilities",
    ];

    // Test web::view_needs_are_capabilities (FEAT-115 R-1)
    #[test]
    fn view_needs_are_capabilities() {
        let caps = katalogr_dist::contract::CAPABILITIES;
        let m = katalogr_web::manifest::manifest();
        let mut bad = Vec::new();
        for v in &m.views {
            let mut needs: Vec<(String, &str)> = Vec::new();
            if let Some(n) = &v.needs {
                needs.push((v.id.clone(), n));
            }
            for p in &v.panels {
                if let Some(n) = &p.needs {
                    needs.push((format!("{}/{}", v.id, p.id), n));
                }
            }
            for (at, n) in needs {
                if !caps.contains(&n) && !PLANNED.contains(&n) {
                    bad.push(format!(
                        "{at} needs {n:?}, which is neither a capability nor planned"
                    ));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}
