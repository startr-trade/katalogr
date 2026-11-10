//! FEAT-033: the timeline and stored-view reads, through the CLI and the HTTP API. The project is
//! a synthetic repository of `.tx` files.

#[path = "views_support/mod.rs"]
mod support;

mod timeline {
    use super::support::*;
    use serde_json::Value;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    fn commit_at(repo: &Path, msg: &str, date: &str) {
        git(repo, &["add", "-A"]);
        let st = Command::new("git")
            .current_dir(repo)
            .args(["commit", "-q", "-m", msg])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .status()
            .unwrap();
        assert!(st.success());
    }

    fn points(repo: &Path, extra: &[&str]) -> Value {
        let mut args = vec!["--json", "view", "timeline"];
        args.extend_from_slice(extra);
        serde_json::from_str(&ok(repo, &args)).expect("timeline JSON")
    }

    /// Stores one entry by hand, with a position whose commit is `commit`.
    fn plant(kx: &Path, n: u32, commit: Option<&str>, age_secs: u64) -> String {
        let id = format!("v1-{n:032x}");
        let commit = commit.map_or("null".to_string(), |c| format!("\"{c}\""));
        let body = format!(
            r#"{{"schema":1,"view_id":"{id}","positions":[{{"repo":"app","commit":{commit},"dirty":null}}],"query":{{"op":"structure","args":{{"n":"{n}"}}}},"mermaid":"x"}}"#
        );
        let path = stored_path(kx, &id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(age_secs))
            .unwrap();
        id
    }

    fn snapshot_of(repo: &Path, tmp: &Path, name: &str) -> PathBuf {
        let docs = tmp.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("a.md"),
            "# T\n\n```katalogr-query\nstructure\n```\n",
        )
        .unwrap();
        let out = tmp.join("snaps").join(name);
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        ok(
            repo,
            &[
                "view",
                "snapshot",
                "--docs",
                docs.to_str().unwrap(),
                "--out",
                out.to_str().unwrap(),
            ],
        );
        out
    }

    // ---- server helpers ----

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

    fn serve(repo: &Path, extra: &[&str]) -> Server {
        let log = repo.join("../api.log");
        let _ = std::fs::remove_file(&log);
        let mut child = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(repo)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(["api", "--listen", "127.0.0.1:0"])
            .args(extra)
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

    fn get(s: &Server, path: &str) -> (u16, String) {
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
        (status, body.to_string())
    }

    // ---- R-13 ----

    #[test]
    fn lists_stored_views() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let v = view_json(&repo, &["structure"]);
        let t = points(&repo, &[]);
        let pts = t["result"]["points"].as_array().unwrap();
        assert_eq!(pts.len(), 1, "{t}");
        assert_eq!(pts[0]["source"], "stored-view");
        assert_eq!(pts[0]["view_id"], v["view_id"]);
        assert_eq!(pts[0]["query"]["op"], "structure");
        assert!(pts[0]["committed"].is_i64(), "{t}");
        assert!(pts[0]["snapshot"].is_null());
        assert!(t.get("partial").is_none());
    }

    #[test]
    fn lists_snapshots() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let one = snapshot_of(&repo, tmp.path(), "rel-1");
        // The folder itself, and a folder of folders, both work.
        for given in [&one, &one.parent().unwrap().to_path_buf()] {
            let t = points(&repo, &["--snapshots", given.to_str().unwrap()]);
            let pts = t["result"]["points"].as_array().unwrap();
            assert_eq!(pts.len(), 1, "{t}");
            assert_eq!(pts[0]["source"], "snapshot");
            assert_eq!(pts[0]["snapshot"], "rel-1");
        }
    }

    #[test]
    fn no_unstored_position() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        view_json(&repo, &["structure"]);
        std::fs::write(repo.join("extra.tx"), "sym fn extra\n").unwrap();
        commit_at(&repo, "c2", "2021-01-01T00:00:00Z");
        ok(&repo, &["index"]);
        // The new commit has no stored view, so it is not a point.
        let t = points(&repo, &[]);
        assert_eq!(t["result"]["points"].as_array().unwrap().len(), 1, "{t}");
    }

    #[test]
    fn order_by_commit_time() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        // The first commit is stored first but dated later than the second.
        let first = view_json(&repo, &["structure"]);
        std::fs::write(repo.join("extra.tx"), "sym fn extra\n").unwrap();
        commit_at(&repo, "c2", "2001-01-01T00:00:00Z");
        ok(&repo, &["index"]);
        let second = view_json(&repo, &["structure"]);
        let t = points(&repo, &[]);
        let pts = t["result"]["points"].as_array().unwrap();
        assert_eq!(pts.len(), 2, "{t}");
        assert_eq!(pts[0]["view_id"], second["view_id"]);
        assert_eq!(pts[0]["committed"], 978_307_200);
        assert_eq!(pts[1]["view_id"], first["view_id"]);
        assert!(pts[1]["committed"].as_i64().unwrap() > 978_307_200);
    }

    #[test]
    fn no_date_when_commit_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = project(tmp.path());
        view_json(&repo, &["structure"]);
        let id = plant(&kx, 7, Some(&"a".repeat(40)), 10);
        let t = points(&repo, &[]);
        let pts = t["result"]["points"].as_array().unwrap();
        assert_eq!(pts.len(), 2, "{t}");
        // A point with no date sorts first and is still listed.
        assert_eq!(pts[0]["view_id"], id.as_str());
        assert!(pts[0]["committed"].is_null());
        assert!(pts[1]["committed"].is_i64());
    }

    #[test]
    fn partial_over_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = project(tmp.path());
        for n in 1..=3 {
            plant(&kx, n, None, 100 - u64::from(n));
        }
        let t = points(&repo, &["--limit", "2"]);
        assert_eq!(t["result"]["points"].as_array().unwrap().len(), 2, "{t}");
        let p = &t["partial"][0];
        assert_eq!(p["cap"], "limit", "{t}");
        assert_eq!(p["at"], 2);
        assert_eq!(p["total"], 3);
        let all = points(&repo, &[]);
        assert!(all.get("partial").is_none());
    }

    #[test]
    fn caps_stored_views() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = project(tmp.path());
        // Five entries; the oldest by file time is unreadable, and is never opened.
        let old = plant(&kx, 1, None, 500);
        std::fs::write(stored_path(&kx, &old), "not json").unwrap();
        let f = std::fs::File::options()
            .write(true)
            .open(stored_path(&kx, &old))
            .unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(500))
            .unwrap();
        let keep: Vec<String> = (2..=5)
            .map(|n| plant(&kx, n, None, 400 - u64::from(n)))
            .collect();
        let t = points(&repo, &["--limit", "3"]);
        let ids: Vec<&str> = t["result"]["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["view_id"].as_str().unwrap())
            .collect();
        // The three newest by file time, in view-ID order (none has a date).
        let mut want: Vec<&str> = keep[1..].iter().map(String::as_str).collect();
        want.sort();
        assert_eq!(ids, want, "{t}");
        assert_eq!(t["partial"][0]["total"], 5, "{t}");
    }

    #[test]
    fn unreadable_snapshot_warns() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let bad = tmp.path().join("snaps").join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("index.json"), "{ not json").unwrap();
        let out = cli(
            &repo,
            &[
                "--json",
                "view",
                "timeline",
                "--snapshots",
                bad.to_str().unwrap(),
            ],
        );
        assert!(out.status.success(), "{out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("snapshot bad skipped"), "{stderr}");
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(v["result"]["points"].as_array().unwrap().is_empty());
    }

    // ---- R-16 ----

    #[test]
    fn snapshot_cannot_leave_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let snap = tmp.path().join("snaps").join("evil");
        std::fs::create_dir_all(&snap).unwrap();
        // A readable view outside the folder, named by a traversal in the index.
        let outside = format!("v1-{:032x}", 9);
        std::fs::write(
            tmp.path().join("snaps").join("secret.json"),
            format!(r#"{{"view_id":"{outside}","positions":[],"query":{{"op":"x","args":{{}}}}}}"#),
        )
        .unwrap();
        let index = r#"{"schema":1,"positions":[],"repos":[],"blocks":[
            {"doc":"a.md","line":1,"source":"s","view_id":"../secret","partial":false},
            {"doc":"a.md","line":2,"source":"s","view_id":"v1-/../../secret","partial":false}]}"#;
        std::fs::write(snap.join("index.json"), index).unwrap();
        let out = cli(
            &repo,
            &[
                "--json",
                "view",
                "timeline",
                "--snapshots",
                snap.to_str().unwrap(),
            ],
        );
        assert!(out.status.success(), "{out:?}");
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(v["result"]["points"].as_array().unwrap().is_empty(), "{v}");
        // The stored-view read refuses the same names.
        let r = cli(
            &repo,
            &[
                "view",
                "stored",
                "../secret",
                "--snapshots",
                snap.to_str().unwrap(),
            ],
        );
        assert_eq!(r.status.code(), Some(2), "{r:?}");
        // A symlinked child folder is not followed. Unix only: creating a symlink on Windows
        // needs Developer Mode or an elevated shell, which CI runners do not grant.
        #[cfg(unix)]
        {
            let real = snapshot_of(&repo, tmp.path(), "real");
            let links = tmp.path().join("links");
            std::fs::create_dir_all(&links).unwrap();
            std::os::unix::fs::symlink(&real, links.join("alias")).unwrap();
            let t = points(&repo, &["--snapshots", links.to_str().unwrap()]);
            assert!(
                t["result"]["points"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|p| p["source"] != "snapshot"),
                "{t}"
            );
        }
    }

    // ---- R-14 ----

    #[test]
    fn cli_body_equals_api() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        view_json(&repo, &["structure"]);
        let server = serve(&repo, &[]);
        let (status, body) = get(&server, "/api/v1/timeline");
        assert_eq!(status, 200);
        let cmd = ok(&repo, &["--json", "view", "timeline"]);
        assert_eq!(body, cmd.trim_end());
    }

    #[test]
    fn stored_cli_body_equals_api() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let v = view_json(&repo, &["structure"]);
        let id = v["view_id"].as_str().unwrap().to_string();
        let server = serve(&repo, &[]);
        let (status, body) = get(&server, &format!("/api/v1/stored-view?id={id}"));
        assert_eq!(status, 200, "{body}");
        let cmd = ok(&repo, &["--json", "view", "stored", &id]);
        assert_eq!(body, cmd.trim_end());
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), v);
    }

    #[test]
    fn stored_absent_is_404() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let id = format!("v1-{:032x}", 42);
        let (status, _) = get(&server, &format!("/api/v1/stored-view?id={id}"));
        assert_eq!(status, 404);
        let out = cli(&repo, &["view", "stored", &id]);
        assert_eq!(out.status.code(), Some(4), "{out:?}");
    }

    #[test]
    fn stored_bad_id_is_400() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        for bad in ["nope", "..%2Fsecret", "v1-xyz"] {
            let (status, _) = get(&server, &format!("/api/v1/stored-view?id={bad}"));
            assert_eq!(status, 400, "{bad}");
        }
        let (status, _) = get(&server, "/api/v1/stored-view");
        assert_eq!(status, 400);
    }
}
