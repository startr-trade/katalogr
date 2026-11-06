//! Export benchmarks (FEAT-034 R-13, R-14). Ignored; run in release, one at a time, under the
//! shared heavy-job lock:
//! `flock ~/.cache/heavy-build.lock cargo test -p katalogr --release --test bench_export -- --ignored --nocapture --test-threads=1`.
//!
//! The corpus is 10,000 synthetic `.tx` symbols under `~/.cache/katalogr-scratch/FEAT-034/` (or
//! `KATALOGR_SCRATCH`), never `/tmp`, and is removed afterwards. Each bench prints the load
//! average beside its number.

mod bench {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Instant;

    fn cli(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env("KATALOGR_THREADS", "4")
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
        base.join("FEAT-034")
    }

    /// 100 files of 100 symbols each, each calling the one before it, indexed. Returns the repo.
    fn corpus(root: &Path) -> PathBuf {
        let _ = std::fs::remove_dir_all(root);
        let (kx, repo) = (root.join("kx"), root.join("app"));
        std::fs::create_dir_all(&repo).unwrap();
        for f in 0..100 {
            let mut text = String::new();
            for i in 0..100 {
                text.push_str(&format!("sym fn s{f}_{i}\n"));
                if i > 0 {
                    text.push_str(&format!("link s{f}_{i} calls s{f}_{}\n", i - 1));
                }
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

    /// Exports once at default flags, warm; returns (seconds, bytes).
    fn run(repo: &Path, out: &Path) -> (f64, u64) {
        // Warm: the first export fills the store of generated views.
        ok(repo, &["export", "html", "--out", out.to_str().unwrap()]);
        let start = Instant::now();
        ok(repo, &["export", "html", "--out", out.to_str().unwrap()]);
        (
            start.elapsed().as_secs_f64(),
            std::fs::metadata(out).unwrap().len(),
        )
    }

    /// R-13. At most 60 s, warm, default flags, 10,000 symbols.
    #[test]
    #[ignore]
    fn export_html_10k_symbols() {
        let root = scratch();
        let repo = corpus(&root);
        let (secs, bytes) = run(&repo, &root.join("review.html"));
        println!(
            "export_html_10k_symbols: {secs:.1} s, {bytes} bytes, load {}",
            load().trim()
        );
        let _ = std::fs::remove_dir_all(&root);
        assert!(secs <= 60.0, "export took {secs:.1} s");
    }

    /// R-14. At most 8 MiB at default flags on the same fixture.
    #[test]
    #[ignore]
    fn export_html_size() {
        let root = scratch();
        let repo = corpus(&root);
        let (secs, bytes) = run(&repo, &root.join("review.html"));
        println!(
            "export_html_size: {:.2} MiB ({bytes} bytes) in {secs:.1} s, load {}",
            bytes as f64 / 1048576.0,
            load().trim()
        );
        let _ = std::fs::remove_dir_all(&root);
        assert!(bytes <= 8 * 1024 * 1024, "the file is {bytes} bytes");
    }
}
