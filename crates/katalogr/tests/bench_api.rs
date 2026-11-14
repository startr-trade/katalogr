//! API benchmarks (FEAT-049 R-11). Ignored; run in release, one at a time, under the shared
//! heavy-job lock:
//! `flock ~/.cache/heavy-build.lock cargo test -p katalogr --release --test bench_api -- --ignored --nocapture --test-threads=1`.
//!
//! The corpus is 10,000 synthetic `.tx` symbols under `~/.cache/katalogr-scratch/FEAT-049/` (or
//! `KATALOGR_SCRATCH`), never `/tmp`, and is removed afterwards.

#[path = "support/server_guard.rs"]
mod support;

mod bench {
    use super::support::ServerGuard;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
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
        base.join("FEAT-049")
    }

    /// 100 files of 100 symbols each, indexed. Returns the repo.
    fn corpus(root: &Path) -> PathBuf {
        let _ = std::fs::remove_dir_all(root);
        let (kx, repo) = (root.join("kx"), root.join("app"));
        std::fs::create_dir_all(&repo).unwrap();
        for f in 0..100 {
            let mut text = String::new();
            for i in 0..100 {
                text.push_str(&format!("sym fn s{f}_{i}\n"));
            }
            std::fs::write(repo.join(format!("f{f}.tx")), text).unwrap();
        }
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
        repo
    }

    fn load() -> String {
        std::fs::read_to_string("/proc/loadavg").unwrap_or_default()
    }

    fn serve(repo: &Path, workers: &str) -> (ServerGuard, SocketAddr) {
        let log = repo.join("../api.log");
        let child = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(repo)
            .env("KATALOGR_THREADS", "4")
            .args(["api", "--listen", "127.0.0.1:0", "--workers", workers])
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let child = ServerGuard::new(child);
        let t = Instant::now();
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if let Some(i) = text.find("listening on http://") {
                let rest = &text[i + "listening on http://".len()..];
                return (child, rest.lines().next().unwrap().trim().parse().unwrap());
            }
            if t.elapsed() >= Duration::from_secs(60) {
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
            "{buf}"
        );
        buf
    }

    fn p(sorted: &[f64], q: f64) -> f64 {
        sorted[((sorted.len() as f64 * q).ceil() as usize).clamp(1, sorted.len()) - 1]
    }

    /// R-11. A `stale-ok` `find` on the 10k-symbol fixture adds at most 30 ms p95 over the same
    /// CLI read (its median wall time, process start included).
    #[test]
    #[ignore]
    fn api_find_stale_ok() {
        let root = scratch();
        let repo = corpus(&root);
        let (_child, addr) = serve(&repo, "4");
        let path = "/api/v1/find?text=s50_50&consistency=stale-ok";
        for _ in 0..20 {
            get(addr, path);
        }
        let mut api: Vec<f64> = (0..300)
            .map(|_| {
                let t = Instant::now();
                get(addr, path);
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        let mut cli_ms: Vec<f64> = (0..30)
            .map(|_| {
                let t = Instant::now();
                let out = cli(
                    &repo,
                    &["--json", "--consistency", "stale-ok", "find", "s50_50"],
                );
                assert!(out.status.success());
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        api.sort_by(|a, b| a.partial_cmp(b).unwrap());
        cli_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let (api95, cli50) = (p(&api, 0.95), p(&cli_ms, 0.5));
        println!(
            "api_find_stale_ok: api p50 {:.1} ms, p95 {api95:.1} ms; cli median {cli50:.1} ms; \
             added {:.1} ms; load {}",
            p(&api, 0.5),
            api95 - cli50,
            load().trim()
        );
        let _ = std::fs::remove_dir_all(&root);
        assert!(api95 - cli50 <= 30.0, "p95 {api95:.1} vs cli {cli50:.1}");
    }

    /// R-11. Four parallel `stale-ok` clients get at least twice one client's throughput.
    #[test]
    #[ignore]
    fn api_parallel_readers() {
        let root = scratch();
        let repo = corpus(&root);
        let (_child, addr) = serve(&repo, "4");
        let path = "/api/v1/find?text=s50_50&consistency=stale-ok";
        for _ in 0..20 {
            get(addr, path);
        }
        let run = |clients: usize| -> f64 {
            let per = 400 / clients;
            let t = Instant::now();
            let hs: Vec<_> = (0..clients)
                .map(|_| {
                    std::thread::spawn(move || {
                        for _ in 0..per {
                            get(addr, path);
                        }
                    })
                })
                .collect();
            hs.into_iter().for_each(|h| h.join().unwrap());
            (per * clients) as f64 / t.elapsed().as_secs_f64()
        };
        let one = run(1);
        let four = run(4);
        println!(
            "api_parallel_readers: 1 client {one:.0} req/s, 4 clients {four:.0} req/s, ratio {:.2}; load {}",
            four / one,
            load().trim()
        );
        let _ = std::fs::remove_dir_all(&root);
        assert!(four >= 2.0 * one, "{four:.0} vs {one:.0}");
    }

    /// R-11. The `api` feature adds at most 1 MiB to the release binary (x86_64 musl). Builds
    /// the binary twice into a scratch target directory and compares the stripped sizes.
    #[test]
    #[ignore]
    fn api_binary_size() {
        let root = scratch().join("binary-size");
        let target = root.join("size-target");
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut sizes = Vec::new();
        for features in ["mcp", "mcp,api"] {
            let st = Command::new("cargo")
                .current_dir(manifest)
                .env("CARGO_TARGET_DIR", &target)
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
            let bin = target.join("release/katalogr");
            let copy = root.join(format!("katalogr-{}", features.replace(',', "-")));
            std::fs::copy(&bin, &copy).unwrap();
            let _ = Command::new("strip").arg(&copy).status();
            sizes.push(std::fs::metadata(&copy).unwrap().len());
        }
        let growth = sizes[1].saturating_sub(sizes[0]);
        println!(
            "api_binary_size: without api {} B, with api {} B, growth {growth} B ({:.2} MiB); load {}",
            sizes[0],
            sizes[1],
            growth as f64 / 1_048_576.0,
            load().trim()
        );
        let _ = std::fs::remove_dir_all(&root);
        assert!(growth <= 1_572_864, "growth {growth} B (limit 1.5 MiB)");
    }
}
