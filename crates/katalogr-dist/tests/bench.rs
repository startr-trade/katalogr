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

    fn load() -> String {
        std::fs::read_to_string("/proc/loadavg").unwrap_or_default()
    }

    /// User plus system CPU seconds of this process so far.
    fn cpu_secs() -> f64 {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let rest = stat.rsplit_once(')').map_or("", |(_, r)| r);
        let f: Vec<&str> = rest.split_whitespace().collect();
        // Fields 14 and 15 of the line are utime and stime, in clock ticks of 10 ms.
        let tick = |i: usize| f.get(i).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
        (tick(11) + tick(12)) / 100.0
    }

    /// Cold read, then 9 warm reads of `read_position` with a stat cache on `root`. Prints the
    /// median wall time, CPU time per read and the median run's step times; returns the median.
    fn time_position(label: &str, root: &Path, scratch: &Path) -> f64 {
        use katalogr_engine::position::{read_position_timed, stat_cache_path};
        let (inc, exc) = (vec!["**".to_string()], Vec::new());
        let cache = stat_cache_path(scratch, "bench");
        let _ = std::fs::remove_dir_all(scratch.join("position"));
        let t = Instant::now();
        let (cold, _) = read_position_timed("r", root, &inc, &exc, Some(&cache)).unwrap();
        println!(
            "{label}: cold {:.0} ms ({} changed paths)",
            t.elapsed().as_secs_f64() * 1000.0,
            cold.changed.len()
        );
        let mut runs = Vec::new();
        let cpu0 = cpu_secs();
        for _ in 0..9 {
            let t = Instant::now();
            let (st, steps) = read_position_timed("r", root, &inc, &exc, Some(&cache)).unwrap();
            assert_eq!(st.position, cold.position);
            runs.push((t.elapsed().as_secs_f64() * 1000.0, steps));
        }
        let cpu_ms = (cpu_secs() - cpu0) / 9.0 * 1000.0;
        runs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let (p, s) = runs[4];
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        println!(
            "{label}: warm median {p:.1} ms (cpu user+sys {cpu_ms:.1} ms per read); steps: cache load {:.1}, tracked stats {:.1}, dir stats {:.1}, walk {:.1}, hash {:.1} ms; load {}",
            ms(s.cache_load),
            ms(s.tracked_stats),
            ms(s.dir_stats),
            ms(s.walk),
            ms(s.hash),
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
                std::fs::write(sub.join(format!("f{f:03}.txt")), format!("file {d} {f}\n"))
                    .unwrap();
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
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "-m",
            "x",
        ]);
        // Files younger than the cache by under 2 s are read again (the racy rule).
        std::thread::sleep(std::time::Duration::from_secs(3));
        let scratch = tempfile::tempdir_in(&base).unwrap();
        let ms = time_position("read_position_10k_files", root, scratch.path());
        assert!(ms <= 50.0, "{ms:.1} ms is over the 50 ms budget");
    }

    /// Times `read_position` on an existing clean checkout named by `KATALOGR_POSITION_ROOT`
    /// (a read-only clone is fine). Budget 250 ms for a 35k-file checkout.
    #[test]
    #[ignore]
    fn read_position_clone() {
        let Ok(root) = std::env::var("KATALOGR_POSITION_ROOT") else {
            println!("KATALOGR_POSITION_ROOT is not set; skipped");
            return;
        };
        let base = std::env::var("KATALOGR_SCRATCH")
            .unwrap_or_else(|_| "/home/dev/.cache/katalogr-scratch/FEAT-084".to_string());
        std::fs::create_dir_all(&base).unwrap();
        let scratch = tempfile::tempdir_in(&base).unwrap();
        let ms = time_position(&root, Path::new(&root), scratch.path());
        assert!(ms <= 250.0, "{ms:.1} ms is over the 250 ms budget");
    }
}
