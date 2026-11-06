#![cfg(feature = "web")]
//! FEAT-101: the type lineage page opened against a real `katalogr serve` over HTTP.

#[path = "views_support/mod.rs"]
mod support;

mod web {
    use super::support::*;
    use katalogr_webtest::{HttpBackend, Page};
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

    /// R-13. The lineage page opens against a real server: without a subject it asks for one,
    /// and with one it asks the two lineage ops and draws the subject, not a failure.
    #[test]
    fn type_lineage_opens() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let s = start(&repo);
        let backend = HttpBackend { addr: s.addr };

        let mut bare = Page::open("#/type-lineage", &backend);
        let text = bare.text();
        assert!(
            text.contains("Choose a type, schema element or contract to see where it goes."),
            "{text}"
        );
        assert!(bare.has("lineage-search"), "{text}");
        assert!(
            !text.contains("isn't in this version of katalogr"),
            "{text}"
        );

        let mut page = Page::open("#/type-lineage?symbol=target", &backend);
        let text = page.text();
        assert!(page.has("lineage-subject"), "{text}");
        assert!(!text.contains("isn't answering"), "{text}");
        assert!(!text.contains("reported a problem"), "{text}");
        let requests = page.requests();
        assert!(
            requests.iter().any(|r| r.starts_with("/api/v1/lineage?")),
            "{requests:?}"
        );
        assert!(
            requests
                .iter()
                .any(|r| r.starts_with("/api/v1/views/lineage?")),
            "{requests:?}"
        );
    }
}
