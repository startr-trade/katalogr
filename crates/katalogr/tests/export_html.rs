#![cfg(feature = "web")]
//! FEAT-034: `katalogr export html` against a synthetic `.tx` project. No board exists
//! anywhere.

#[path = "views_support/mod.rs"]
mod support;

mod export_html {
    use super::support::*;
    use katalogr_web::export::{answer_key, plan_views};
    use katalogr_webtest::{NoBackend, Page};
    use serde_json::Value;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        (tmp, repo)
    }

    fn export(repo: &Path, out: &Path, extra: &[&str]) -> std::process::Output {
        let mut args = vec!["export", "html", "--out", out.to_str().unwrap()];
        args.extend_from_slice(extra);
        cli(repo, &args)
    }

    fn export_ok(repo: &Path, out: &Path, extra: &[&str]) -> String {
        let r = export(repo, out, extra);
        assert!(
            r.status.success(),
            "export failed: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        std::fs::read_to_string(out).unwrap()
    }

    /// The `kw-data` block of a file.
    fn data_of(file: &str) -> Value {
        let tag = "<script type=\"application/json\" id=\"kw-data\">";
        let start = file.find(tag).expect("a data block") + tag.len();
        let end = file[start..].find("</script>").unwrap() + start;
        serde_json::from_str(&file[start..end]).expect("the data is JSON")
    }

    #[test]
    fn writes_one_file() {
        let (tmp, repo) = setup();
        let out = tmp.path().join("review.html");
        let r = export(&repo, &out, &[]);
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        let file = std::fs::read_to_string(&out).unwrap();
        assert!(file.starts_with("<!DOCTYPE html>"));
        assert!(file.contains("id=\"kw-data\""));
        assert!(!file.contains("<script src") && !file.contains("rel=\"stylesheet\""));
        let err = String::from_utf8_lossy(&r.stderr);
        assert!(err.contains("wrote") && err.contains("bytes"), "{err}");
        // Nothing else was left beside it.
        let left: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn same_views_as_serve() {
        let (tmp, repo) = setup();
        let file = export_ok(&repo, &tmp.path().join("a.html"), &[]);
        let data = data_of(&file);
        let caps: Vec<String> = data["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_string())
            .collect();
        assert!(!caps.contains(&"web-view".to_string()));
        let answers = data["answers"].as_object().unwrap();
        for call in plan_views(&caps) {
            assert!(answers.contains_key(&answer_key(&call)), "{call:?}");
        }
        // The manifest is the served page's.
        let tag = "<script type=\"application/json\" id=\"kw-views\">";
        let start = file.find(tag).unwrap() + tag.len();
        let end = file[start..].find("</script>").unwrap() + start;
        let embedded: Value = serde_json::from_str(&file[start..end]).unwrap();
        let served: Value = serde_json::from_str(katalogr_web::VIEWS_JSON).unwrap();
        assert_eq!(embedded, served);
        assert_eq!(data["format"], 1);
    }

    #[test]
    fn no_matrix_page() {
        let (tmp, repo) = setup();
        let file = export_ok(&repo, &tmp.path().join("a.html"), &[]);
        let data = data_of(&file);
        assert!(
            data["answers"]
                .as_object()
                .unwrap()
                .keys()
                .all(|k| !k.contains("matrix"))
        );
        let manifest = katalogr_web::manifest::manifest();
        assert!(manifest.views.iter().all(|v| !v.id.contains("matrix")));
        let mut p = Page::open_file(&file, "#/matrix");
        assert!(p.text().contains("There is no such view."), "{}", p.text());
    }

    #[test]
    fn absent_views_say_so() {
        let (tmp, repo) = setup();
        let file = export_ok(&repo, &tmp.path().join("a.html"), &[]);
        let data = data_of(&file);
        let caps: Vec<&str> = data["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        let manifest = katalogr_web::manifest::manifest();
        let absent: Vec<_> = manifest
            .views
            .iter()
            .filter(|v| v.nav && v.needs.as_deref().is_some_and(|n| !caps.contains(&n)))
            .collect();
        for v in absent {
            let mut p = Page::open_file(&file, &format!("#/{}", v.id));
            let t = p.text();
            assert!(t.contains(v.absent.as_deref().unwrap()), "{}: {t}", v.id);
        }
        // A view that is held opens with its data.
        let mut p = Page::open_file(&file, "#/health");
        assert!(p.text().contains("Hubs"));
        assert!(
            !p.text().contains("This isn't in this export"),
            "{}",
            p.text()
        );
    }

    #[test]
    fn diagram_draws_answer_bytes() {
        let (tmp, repo) = setup();
        let file = export_ok(&repo, &tmp.path().join("a.html"), &[]);
        let data = data_of(&file);
        let mermaid = data["answers"]["views/structure"]["body"]["mermaid"]
            .as_str()
            .expect("a mermaid field");
        let mut p = Page::open_file(&file, "#/graph");
        assert_eq!(p.pele_sources(), [mermaid.to_string()]);
    }

    #[test]
    fn same_position_same_bytes() {
        let (tmp, repo) = setup();
        let a = export_ok(&repo, &tmp.path().join("a.html"), &[]);
        let b = export_ok(&repo, &tmp.path().join("b.html"), &[]);
        assert_eq!(a, b);
    }

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

    fn serve(repo: &Path) -> Server {
        let log = repo.join("../api.log");
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

    fn get(s: &Server, path: &str) -> (u16, Value) {
        let req = format!(
            "GET {path} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
            s.addr
        );
        let mut c = TcpStream::connect(s.addr).expect("connect");
        c.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        c.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        let _ = c.read_to_end(&mut buf);
        let text = String::from_utf8_lossy(&buf).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }

    #[test]
    fn answers_equal_api_bodies() {
        let (tmp, repo) = setup();
        let file = export_ok(&repo, &tmp.path().join("a.html"), &[]);
        let data = data_of(&file);
        let server = serve(&repo);
        let mut checked = 0;
        for (key, answer) in data["answers"].as_object().unwrap() {
            if !(key.starts_with("views/")
                || key.starts_with("hubs")
                || key.starts_with("explain")
                || key.starts_with("tests")
                || key.starts_with("coverage"))
            {
                continue;
            }
            let sep = if key.contains('?') { '&' } else { '?' };
            let (status, mut body) = get(&server, &format!("/api/v1/{key}{sep}consistency=wait"));
            if let Some(m) = body.as_object_mut() {
                for k in ["watermark", "context", "pending"] {
                    m.remove(k);
                }
            }
            assert_eq!(answer["status"], status, "{key}");
            assert_eq!(answer["body"], body, "{key}");
            checked += 1;
        }
        assert!(checked >= 8, "only {checked} answers compared");
    }

    #[test]
    fn not_exact_writes_nothing() {
        let (tmp, repo) = setup();
        let out = tmp.path().join("review.html");
        let r = export(&repo, &out, &["--consistency", "stale-ok"]);
        assert_eq!(
            r.status.code(),
            Some(5),
            "{}",
            String::from_utf8_lossy(&r.stderr)
        );
        assert!(!out.exists());
    }

    #[test]
    fn position_unavailable_exit_6() {
        let (tmp, repo) = setup();
        let out = tmp.path().join("review.html");
        let none = "0".repeat(40);
        let r = export(&repo, &out, &["--at", &none]);
        assert_eq!(
            r.status.code(),
            Some(6),
            "{}",
            String::from_utf8_lossy(&r.stderr)
        );
        assert!(!out.exists());
    }

    #[test]
    fn failure_leaves_earlier_file() {
        let (tmp, repo) = setup();
        let out = tmp.path().join("review.html");
        std::fs::write(&out, "earlier").unwrap();
        let none = "0".repeat(40);
        let r = export(&repo, &out, &["--at", &none]);
        assert_eq!(r.status.code(), Some(6));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "earlier");
        let left: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        // A good export replaces it.
        let file = export_ok(&repo, &out, &[]);
        assert!(file.starts_with("<!DOCTYPE html>"));
    }

    #[test]
    fn symbol_cap_marked_partial() {
        let (tmp, repo) = setup();
        let file = export_ok(&repo, &tmp.path().join("a.html"), &["--max-symbols", "2"]);
        let data = data_of(&file);
        assert_eq!(data["symbols"].as_array().unwrap().len(), 2);
        let partial = &data["partial"][0];
        assert_eq!(partial["at"], "export.symbols");
        assert_eq!(partial["shown"], 2);
        assert!(partial["total"].as_u64().unwrap() > 2, "{partial}");
        let mut p = Page::open_file(&file, "#/health");
        assert!(
            p.text()
                .contains("This export holds the 2 most connected of"),
            "{}",
            p.text()
        );
        let _ = &NoBackend;
        // Uncapped, nothing is cut.
        let all = export_ok(&repo, &tmp.path().join("b.html"), &[]);
        assert!(data_of(&all)["partial"].as_array().unwrap().is_empty());
    }

    #[test]
    fn no_source_by_default() {
        let (tmp, repo) = setup();
        let file = export_ok(&repo, &tmp.path().join("a.html"), &[]);
        let data = data_of(&file);
        assert_eq!(data["with_source"], false);
        let keys: Vec<&String> = data["answers"].as_object().unwrap().keys().collect();
        assert!(
            keys.iter()
                .any(|k| k.starts_with("show?") && k.contains("signature=true"))
        );
        assert!(
            keys.iter()
                .all(|k| !k.starts_with("show?") || k.contains("signature=true")),
            "{keys:?}"
        );
    }

    #[test]
    fn with_source_includes_lines() {
        let (tmp, repo) = setup();
        let file = export_ok(&repo, &tmp.path().join("a.html"), &["--with-source"]);
        let data = data_of(&file);
        assert_eq!(data["with_source"], true);
        let keys: Vec<&String> = data["answers"].as_object().unwrap().keys().collect();
        assert!(
            keys.iter()
                .any(|k| k.starts_with("show?") && !k.contains("signature"))
        );
        assert!(
            keys.iter()
                .all(|k| !k.starts_with("show?") || !k.contains("signature")),
            "{keys:?}"
        );
        let shown = data["answers"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(k, _)| k.starts_with("show?"))
            .map(|(_, a)| a["body"].to_string())
            .unwrap();
        assert!(shown.contains("sym fn"), "{shown}");
    }

    #[test]
    fn no_absolute_paths() {
        let (tmp, repo) = setup();
        let file = export_ok(&repo, &tmp.path().join("a.html"), &[]);
        for p in [
            repo.to_str().unwrap(),
            tmp.path().to_str().unwrap(),
            env!("CARGO_MANIFEST_DIR"),
        ] {
            assert!(!file.contains(p), "the file holds {p}");
        }
    }

    #[test]
    fn prints_repos_named() {
        let (tmp, repo) = setup();
        let r = export(&repo, &tmp.path().join("a.html"), &[]);
        let err = String::from_utf8_lossy(&r.stderr);
        assert!(err.contains("repos named: app"), "{err}");
        assert!(err.contains("bytes"), "{err}");
    }

    #[test]
    fn runs_without_board() {
        let (tmp, repo) = setup();
        let empty = tempfile::tempdir().unwrap();
        let out = tmp.path().join("a.html");
        let r = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(&repo)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env("KANBANR_DATA_DIR", empty.path())
            .args(["export", "html", "--out", out.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        assert!(out.exists());
    }
}
