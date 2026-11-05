//! Evaluation benchmarks (FEAT-061 R-11). Ignored; run in release:
//! `cargo test -p katalogr-dist --release --test bench -- --ignored --nocapture`.

mod bench {
    use katalogr_dist::eval::{EvalRequest, parse_sweep, run_fixtures, sweep};
    use std::path::Path;
    use std::time::Instant;

    fn request() -> EvalRequest {
        EvalRequest {
            sets: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/eval")],
            ..EvalRequest::default()
        }
    }

    #[test]
    #[ignore]
    fn eval_fixture_suite() {
        let t = Instant::now();
        let r = run_fixtures(&request()).unwrap();
        let ms = t.elapsed().as_millis();
        println!(
            "eval_fixture_suite: {ms} ms for {} queries",
            r.queries.len()
        );
        assert!(ms < 5000, "{ms} ms");
    }

    #[test]
    #[ignore]
    fn eval_sweep_16() {
        let grid = [
            parse_sweep("search.k1=1.0,1.2,1.5,2.0").unwrap(),
            parse_sweep("search.b=0.5,0.6,0.75,0.9").unwrap(),
        ];
        let t = Instant::now();
        let r = sweep(&request(), &grid, "retrieval.ndcg@10").unwrap();
        let ms = t.elapsed().as_millis();
        println!("eval_sweep_16: {ms} ms for {} combinations", r.rows.len());
        assert_eq!(r.rows.len(), 16);
        assert!(ms < 10000, "{ms} ms");
    }

    fn median_ms(mut v: Vec<f64>) -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }

    fn load() -> String {
        std::fs::read_to_string("/proc/loadavg").unwrap_or_default()
    }

    /// Times `read_position` (warm, median of 9), `gix` status alone and `git status` on `root`.
    fn time_position(label: &str, root: &Path) -> f64 {
        use katalogr_engine::position::{read_position, status_path_count};
        let (inc, exc) = (vec!["**".to_string()], Vec::new());
        read_position("r", root, &inc, &exc).unwrap(); // warm the file cache
        let mut pos = Vec::new();
        let mut status = Vec::new();
        let mut cli = Vec::new();
        for _ in 0..9 {
            let t = Instant::now();
            let st = read_position("r", root, &inc, &exc).unwrap();
            pos.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(st.position.commit.is_some());
            let t = Instant::now();
            status_path_count(root).unwrap();
            status.push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            let ok = std::process::Command::new("git")
                .current_dir(root)
                .args(["status", "--porcelain", "-uall"])
                .output()
                .unwrap();
            assert!(ok.status.success());
            cli.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let p = median_ms(pos);
        println!(
            "{label}: read_position {p:.1} ms, gix status only {:.1} ms, git status {:.1} ms; load {}",
            median_ms(status),
            median_ms(cli),
            load().trim()
        );
        p
    }

    /// FEAT-084 R-12: `read_position` adds at most 50 ms to a read on a 10k-file repo.
    #[test]
    #[ignore]
    fn read_position_10k_files() {
        let base = std::env::var("KATALOGR_SCRATCH")
            .unwrap_or_else(|_| "/home/dev/.cache/katalogr-scratch/FEAT-084".to_string());
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(&base).unwrap();
        let root = dir.path();
        for d in 0..100 {
            let sub = root.join(format!("d{d:03}"));
            std::fs::create_dir_all(&sub).unwrap();
            for f in 0..100 {
                std::fs::write(sub.join(format!("f{f:03}.txt")), format!("file {d} {f}\n")).unwrap();
            }
        }
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .current_dir(root)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .status()
                .unwrap();
            assert!(st.success());
        };
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&[
            "-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-q", "-m", "x",
        ]);
        let ms = time_position("read_position_10k_files", root);
        assert!(ms <= 50.0, "{ms:.1} ms is over the 50 ms budget");
    }

    /// Times `read_position` on an existing clean checkout named by `KATALOGR_POSITION_ROOT`.
    #[test]
    #[ignore]
    fn read_position_clone() {
        let Ok(root) = std::env::var("KATALOGR_POSITION_ROOT") else {
            println!("KATALOGR_POSITION_ROOT is not set; skipped");
            return;
        };
        time_position(&root, Path::new(&root));
    }
}
