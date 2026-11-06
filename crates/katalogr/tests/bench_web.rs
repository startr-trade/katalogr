#![cfg(feature = "web")]
//! Web view benchmarks (FEAT-033 R-20 to R-22). Ignored; run in release, one at a time, under
//! the shared heavy-job lock:
//! `flock ~/.cache/heavy-build.lock cargo test -p katalogr --release --test bench_web -- --ignored --nocapture --test-threads=1`.
//!
//! The corpus is 10,000 synthetic `.tx` symbols under `~/.cache/katalogr-scratch/FEAT-033/` (or
//! `KATALOGR_SCRATCH`), never `/tmp`, and is removed afterwards. Each bench prints the load
//! average beside its result.

mod bench {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    fn cli(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .output()
            .expect("run katalogr")
    }

    fn ok(dir: &Path, args: &[&str]) {
        let out = cli(dir, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn scratch() -> PathBuf {
        let base = std::env::var_os("KATALOGR_SCRATCH")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").expect("HOME"))
                    .join(".cache/katalogr-scratch")
            });
        base.join("FEAT-033")
    }

    /// 100 files of 100 symbols each, a git repository with one commit, indexed. Returns
    /// (kindex, repo).
    fn corpus(root: &Path) -> (PathBuf, PathBuf) {
        let _ = std::fs::remove_dir_all(root);
        let (kx, repo) = (root.join("kx"), root.join("app"));
        std::fs::create_dir_all(&repo).unwrap();
        for f in 0..100 {
            let mut text = String::new();
            for i in 0..100 {
                text.push_str(&format!("sym fn s{f}_{i}\n"));
            }
            for i in 1..100 {
                text.push_str(&format!("link s{f}_{i} calls s{f}_{}\n", i - 1));
            }
            std::fs::write(repo.join(format!("f{f}.tx")), text).unwrap();
        }
        let git = |args: &[&str]| {
            let st = Command::new("git")
                .current_dir(&repo)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .args(args)
                .status()
                .unwrap();
            assert!(st.success());
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "c1"]);
        ok(root, &["kindex", "init", kx.to_str().unwrap()]);
        let spec = format!("app={}", repo.display());
        ok(
            root,
            &[
                "--kindex",
                kx.to_str().unwrap(),
                "project",
                "init",
                "demo",
                "--repo",
                &spec,
                "--mark",
            ],
        );
        ok(&repo, &["index"]);
        (kx, repo)
    }

    fn load() -> String {
        std::fs::read_to_string("/proc/loadavg").unwrap_or_default()
    }

    // The caller owns the child and reaps it; the lint cannot follow it out of this function.
    #[allow(clippy::zombie_processes)]
    fn serve(repo: &Path) -> (Child, SocketAddr) {
        let log = repo.join("../serve.log");
        let mut child = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(repo)
            .env("KATALOGR_THREADS", "4")
            .args(["serve", "--listen", "127.0.0.1:0", "--workers", "4"])
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let t = Instant::now();
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if let Some(i) = text.find("listening on http://") {
                let rest = &text[i + "listening on http://".len()..];
                return (child, rest.lines().next().unwrap().trim().parse().unwrap());
            }
            if t.elapsed() >= Duration::from_secs(60) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("no start: {text}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn get(addr: SocketAddr, path: &str) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(format!("GET {path} HTTP/1.0\r\nHost: {addr}\r\n\r\n").as_bytes())
            .unwrap();
        let mut buf = String::new();
        s.read_to_string(&mut buf).unwrap();
        assert!(
            buf.starts_with("HTTP/1.0 200") || buf.starts_with("HTTP/1.1 200"),
            "{path}: {}",
            &buf[..buf.len().min(300)]
        );
        buf
    }

    fn timed(addr: SocketAddr, path: &str, n: usize) -> Vec<f64> {
        let mut v: Vec<f64> = (0..n)
            .map(|_| {
                let t = Instant::now();
                get(addr, path);
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    }

    fn p(sorted: &[f64], q: f64) -> f64 {
        sorted[((sorted.len() as f64 * q).ceil() as usize).clamp(1, sorted.len()) - 1]
    }

    /// R-20. The `web` feature adds at most 1 MiB to the stripped release binary. Builds the
    /// binary twice into the worktree's own `target/`, the `web` build last, so the binary the
    /// other benches use is the default one.
    #[test]
    #[ignore]
    fn web_binary_size() {
        let root = scratch();
        std::fs::create_dir_all(&root).unwrap();
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut sizes = Vec::new();
        for features in ["mcp,api", "mcp,api,web"] {
            let st = Command::new("cargo")
                .current_dir(manifest)
                .args([
                    "build",
                    "--release",
                    "-j4",
                    "-p",
                    "katalogr",
                    "--no-default-features",
                    "--features",
                    features,
                ])
                .status()
                .expect("cargo build");
            assert!(st.success());
            let bin = Path::new(env!("CARGO_BIN_EXE_katalogr"));
            let copy = root.join(format!("katalogr-{}", features.replace(',', "-")));
            std::fs::copy(bin, &copy).unwrap();
            let _ = Command::new("strip").arg(&copy).status();
            sizes.push(std::fs::metadata(&copy).unwrap().len());
        }
        let growth = sizes[1].saturating_sub(sizes[0]);
        println!(
            "web_binary_size: without web {} B, with web {} B, growth {growth} B ({:.2} MiB); load {}",
            sizes[0],
            sizes[1],
            growth as f64 / 1_048_576.0,
            load().trim()
        );
        let _ = std::fs::remove_dir_all(&root);
        assert!(growth <= 1_048_576, "growth {growth} B");
    }

    /// R-21. `GET /assets/pele.global.js` answers in at most 5 ms p95 on loopback, idle server.
    #[test]
    #[ignore]
    fn web_asset_hit() {
        let root = scratch();
        let (_kx, repo) = corpus(&root);
        let (mut child, addr) = serve(&repo);
        timed(addr, "/assets/pele.global.js", 50);
        let v = timed(addr, "/assets/pele.global.js", 1000);
        let p95 = p(&v, 0.95);
        println!(
            "web_asset_hit: p50 {:.2} ms, p95 {p95:.2} ms, max {:.2} ms; load {}",
            p(&v, 0.5),
            v[v.len() - 1],
            load().trim()
        );
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&root);
        assert!(p95 <= 5.0, "p95 {p95:.2} ms");
    }

    /// R-22. `timeline` with 1,000 stored views answers in at most 300 ms (median of 20).
    #[test]
    #[ignore]
    fn timeline_1000_points() {
        let root = scratch();
        let (kx, repo) = corpus(&root);
        let commit = {
            let out = Command::new("git")
                .current_dir(&repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        for n in 0..1000u32 {
            let id = format!("v1-{n:032x}");
            let dir = kx.join("projects/demo/.cache/views").join(&id[3..5]);
            std::fs::create_dir_all(&dir).unwrap();
            let body = format!(
                r#"{{"schema":1,"view_format":1,"view_id":"{id}","exact":true,"positions":[{{"repo":"app","commit":"{commit}","dirty":null}}],"query":{{"op":"structure","args":{{"max-nodes":"{n}"}}}},"mermaid":"flowchart LR\n","graph":{{}},"pele":"1.7.0"}}"#
            );
            std::fs::write(dir.join(format!("{id}.json")), body).unwrap();
        }
        let (mut child, addr) = serve(&repo);
        timed(addr, "/api/v1/timeline", 3);
        let v = timed(addr, "/api/v1/timeline", 20);
        let body = get(addr, "/api/v1/timeline");
        let points = body.matches("\"view_id\"").count();
        let med = p(&v, 0.5);
        println!(
            "timeline_1000_points: {points} points, median {med:.1} ms, p95 {:.1} ms; load {}",
            p(&v, 0.95),
            load().trim()
        );
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(points, 1000);
        assert!(med <= 300.0, "median {med:.1} ms");
    }

    /// R-22. A view page's own requests, against a warm store, each cost at most 100 ms more
    /// than a stored view costs over reading the position (FEAT-088's 20 ms): p95 of each page
    /// request is compared with `position` p95 + 20 + 100 ms.
    #[test]
    #[ignore]
    fn web_view_wait() {
        let root = scratch();
        let (_kx, repo) = corpus(&root);
        let (mut child, addr) = serve(&repo);
        let wait = "consistency=wait&timeout=10";
        // Warm: the first reads compute and store.
        get(addr, &format!("/api/v1/views/structure?{wait}"));
        get(addr, &format!("/api/v1/views/callers?{wait}&symbol=s50_50"));
        let position = p(&timed(addr, "/api/v1/position", 50), 0.95);
        let limit = position + 20.0 + 100.0;
        let mut worst = 0.0f64;
        for path in [
            "/api/v1/meta".to_string(),
            "/api/v1/views/".to_string(),
            format!("/api/v1/views/structure?{wait}"),
            format!("/api/v1/views/callers?{wait}&symbol=s50_50"),
            "/api/v1/timeline".to_string(),
            "/api/v1/coverage".to_string(),
        ] {
            let v = timed(addr, &path, 50);
            let p95 = p(&v, 0.95);
            worst = worst.max(p95);
            println!(
                "web_view_wait: {path}: p50 {:.1} ms, p95 {p95:.1} ms (limit {limit:.1}, position p95 {position:.1}); load {}",
                p(&v, 0.5),
                load().trim()
            );
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&root);
        assert!(worst <= limit, "worst p95 {worst:.1} ms over {limit:.1} ms");
    }
}
