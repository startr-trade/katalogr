#![cfg(feature = "web")]
//! FEAT-033: the link contract with kanbanr, and the page's own scripts driven against a real
//! `katalogr serve` over HTTP (`HttpBackend` and `Page`).

#[path = "views_support/mod.rs"]
mod support;

mod web {
    use super::support::*;
    use katalogr_webtest::{HttpBackend, Page};
    use serde_json::{Value, json};
    use std::net::SocketAddr;
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

    fn start(repo: &Path) -> Server {
        let log = repo.join("../serve.log");
        let mut child = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(repo)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(["serve", "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .expect("spawn katalogr serve");
        let started = Instant::now();
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if let Some(i) = text.find("listening on http://") {
                let rest = &text[i + "listening on http://".len()..];
                let addr = rest.lines().next().unwrap().trim().parse().unwrap();
                return Server { child, addr };
            }
            if let Ok(Some(st)) = child.try_wait() {
                panic!("katalogr serve exited early ({st}): {text}");
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "no start: {text}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn links_file() -> Value {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/contract/web-links-v1.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn hash_of(link: &str) -> &str {
        link.strip_prefix('/').expect("a link starts with /")
    }

    /// R-1. The contract kanbanr copies: pinned in full, so a change is deliberate.
    #[test]
    fn links_shared_file_matches() {
        let file = links_file();
        let pinned = json!({
            "format": 1,
            "capability": "web-view",
            "template": "/#/{view}?at={at}&symbol={symbol}",
            "views": ["graph", "impact", "service-map"],
            "examples": [
                {"link": "/#/graph?symbol=target", "view": "graph", "at": "", "symbol": "target"},
                {"link": "/#/service-map?symbol=target", "view": "service-map", "at": "", "symbol": "target"},
                {"link": "/#/impact?at=0123456789abcdef0123456789abcdef01234567&symbol=repo%3Aapp%23fn%3Atarget",
                 "view": "impact", "at": "0123456789abcdef0123456789abcdef01234567", "symbol": "repo:app#fn:target"}
            ]
        });
        assert_eq!(file, pinned);
        // Nothing machine-specific: no absolute path and no host.
        let text = file.to_string();
        assert!(
            !text.contains("localhost") && !text.contains("127.0.0.1") && !text.contains("/home/")
        );
        // The capability is the one the server reports, and the views are the manifest's.
        assert_eq!(file["capability"], katalogr_dist::contract::WEB_CAPABILITY);
        let mut ids: Vec<String> = katalogr_web::manifest::manifest()
            .views
            .into_iter()
            .filter(|v| v.link)
            .map(|v| v.id)
            .collect();
        ids.sort();
        assert_eq!(file["views"], json!(ids));
        // Every example is what the page's own link builder makes, and parses back.
        let mut page = Page::open("#/graph", &katalogr_webtest::MapBackend::new());
        for e in file["examples"].as_array().unwrap() {
            let (view, at, symbol) = (
                e["view"].as_str().unwrap(),
                e["at"].as_str().unwrap(),
                e["symbol"].as_str().unwrap(),
            );
            let built = page.eval_json(&format!(
                "KW.core.buildLink({}, {{at: {}, symbol: {}}})",
                json!(view),
                json!(at),
                json!(symbol)
            ));
            let built: String = serde_json::from_str(&built).unwrap();
            assert_eq!(format!("/{built}"), e["link"].as_str().unwrap());
            let parsed = page.eval_json(&format!("KW.core.parseRoute({})", json!(built)));
            let parsed: Value = serde_json::from_str(&parsed).unwrap();
            assert_eq!(parsed["view"], view);
            assert_eq!(parsed["params"]["symbol"], symbol);
        }
    }

    /// R-1. Each example link opens its view against a real server.
    #[test]
    fn kept_views_open_by_link() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let s = start(&repo);
        let backend = HttpBackend { addr: s.addr };
        let titles = [
            ("graph", "Graph explorer"),
            ("service-map", "Service map"),
            ("impact", "Impact"),
        ];
        for e in links_file()["examples"].as_array().unwrap() {
            let view = e["view"].as_str().unwrap();
            let mut page = Page::open(hash_of(e["link"].as_str().unwrap()), &backend);
            let text = page.text();
            let title = titles.iter().find(|(v, _)| *v == view).unwrap().1;
            assert!(text.contains(title), "{view}: {text}");
            assert!(!text.contains("isn't answering"), "{view}: {text}");
            assert!(!text.contains("reported a problem"), "{view}: {text}");
            if view != "impact" {
                assert!(!page.pele_sources().is_empty(), "{view}: {text}");
            }
        }
    }

    /// R-8. The diagram is drawn from the very Mermaid `view` prints.
    #[test]
    fn mermaid_equals_view_command() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let s = start(&repo);
        let mut page = Page::open("#/graph", &HttpBackend { addr: s.addr });
        let cmd = view_json(&repo, &["structure"]);
        assert_eq!(page.pele_sources(), [cmd["mermaid"].as_str().unwrap()]);
        let mut callers = Page::open("#/graph?symbol=target", &HttpBackend { addr: s.addr });
        let cmd = view_json(&repo, &["callers", "target"]);
        assert_eq!(callers.pele_sources(), [cmd["mermaid"].as_str().unwrap()]);
    }

    /// R-10. A view the server cannot fill still opens, says so, and asks for nothing else.
    #[test]
    fn absent_views_still_open() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let s = start(&repo);
        let backend = HttpBackend { addr: s.addr };
        for (hash, sentence) in [(
            "#/dependencies",
            "Dependency and vulnerability data isn't in this version of katalogr.",
        )] {
            let mut page = Page::open(hash, &backend);
            assert!(page.text().contains(sentence), "{hash}: {}", page.text());
            assert_eq!(page.requests(), ["/api/v1/meta"], "{hash}");
        }
    }

    /// R-12. The health panels find their rows in the real answers.
    #[test]
    fn health_panels_find_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let s = start(&repo);
        let mut page = Page::open("#/health", &HttpBackend { addr: s.addr });
        let text = page.text();
        for panel in ["hubs", "cycles", "communities", "dead-code", "coverage"] {
            assert!(page.has(&format!("panel-{panel}")), "{panel}");
        }
        // The panels with rows draw a table from `result.items` / `result.by_extension`; the
        // others (hubs, communities) say they are empty, which is the same path walked to an empty list.
        for want in [
            "Members",
            "app:ring1.tx, app:ring2.tx",
            "app:lib.tx#fn:dup",
            "Parse errors",
        ] {
            assert!(text.contains(want), "{want} missing: {text}");
        }
        for empty in [
            "No hubs in this index.",
            "No communities found in this index.",
        ] {
            assert!(text.contains(empty), "{empty} missing: {text}");
        }
        assert!(!text.contains("reported a problem"), "{text}");
        assert!(!text.contains("Nothing is indexed yet."), "{text}");
    }

    /// R-12. Every op the manifest calls, and every parameter it sends, is in the OpenAPI document.
    #[test]
    fn manifest_calls_valid_ops() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let s = start(&repo);
        let doc: Value = {
            let mut p = Page::open("#/graph", &HttpBackend { addr: s.addr });
            let _ = p.text();
            let body = katalogr_webtest::HttpBackend { addr: s.addr };
            use katalogr_webtest::Backend;
            let (status, text) = body.get("/api/v1/openapi.json");
            assert_eq!(status, 200);
            serde_json::from_str(&text).unwrap()
        };
        let params_of = |op: &str| -> Vec<String> {
            let item = &doc["paths"][format!("/api/v1/{op}")]["get"];
            assert!(item.is_object(), "{op} is not in the OpenAPI document");
            item["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["name"].as_str().unwrap().to_string())
                .collect()
        };
        for view in katalogr_web::manifest::manifest().views {
            for panel in view.panels {
                let Some(op) = panel.op else { continue };
                let known = params_of(&op);
                for name in panel.params.keys() {
                    assert!(
                        known.contains(name),
                        "{}: {op} has no parameter {name}",
                        panel.id
                    );
                }
            }
        }
        // The routes the views and the symbol page call by name.
        for op in [
            "meta",
            "timeline",
            "stored-view",
            "explain",
            "why",
            "tests",
            "show",
            "search",
            "views/",
            "views/structure",
            "views/callers",
            "views/contracts",
        ] {
            if op != "meta" {
                params_of(op);
            }
        }
    }
}
