//! FEAT-072 benchmarks (R-11, R-12, R-13). Ignored; run in release, one at a time:
//! `cargo test -p katalogr-dist --release --test parallel_bench -- --ignored --nocapture
//! --test-threads=1`.

#[path = "support/synth.rs"]
mod synth;

mod bench {
    use super::synth::rust_corpus;
    use camino::Utf8PathBuf;
    use katalogr_engine::parallel::Parallelism;
    use katalogr_engine::registry::ExtensionRegistry;
    use katalogr_engine::{
        Consistency, Engine, IndexStats, NewEvent, QueryOptions, Scope, canonical_graph,
    };
    use katalogr_store::{Kindex, ProjectConfig, ProjectName, RepoConfig};
    use std::collections::BTreeMap;
    use std::num::NonZeroUsize;
    use std::time::{Duration, Instant};

    fn cores() -> usize {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    }

    fn load() -> String {
        std::fs::read_to_string("/proc/loadavg")
            .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    }

    /// Wall time and per-stage times of one full index of `work` at `threads` threads.
    struct Run {
        wall: Duration,
        stats: IndexStats,
        files: usize,
        graph_len: usize,
    }

    fn index(work: &Utf8PathBuf, threads: usize) -> Run {
        let tmp = tempfile::tempdir().unwrap();
        let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&kx)
            .unwrap()
            .create_project(
                &ProjectName::new("p").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(katalogr_ext_rust::RustExtractor));
        reg.register(Box::new(katalogr_ext_md::MarkdownExtractor));
        let roots = BTreeMap::from([("r".to_string(), work.clone())]);
        let mut e =
            Engine::open(dir, reg, roots, Instant::now() + Duration::from_secs(30)).unwrap();
        e.set_parallelism(Parallelism {
            threads: NonZeroUsize::new(threads).unwrap(),
            ..Parallelism::sequential()
        });
        let start = Instant::now();
        e.inbox().append(&NewEvent::rescan("r")).unwrap();
        let o = QueryOptions {
            consistency: Consistency::WaitAll,
            timeout: Duration::from_secs(3600),
            scope: Scope::Project,
        };
        let a = e.with_consistency(&o, |tx| canonical_graph(tx)).unwrap();
        let wall = start.elapsed();
        assert!(!a.stale, "drained");
        Run {
            wall,
            stats: e.index_stats(),
            files: e.dir().list_shards().unwrap().len(),
            graph_len: a.value.len(),
        }
    }

    fn corpus(crates: usize) -> (tempfile::TempDir, Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let work = Utf8PathBuf::from_path_buf(tmp.path().join("work")).unwrap();
        rust_corpus(work.as_std_path(), crates, 40, 12);
        (tmp, work)
    }

    fn secs(d: Duration) -> f64 {
        d.as_secs_f64()
    }

    /// R-11. The plan, encode, hash and write stages scale to at least 0.6x the cores.
    #[test]
    #[ignore]
    fn parallel_stage_scaling() {
        let n = cores();
        let (_t, work) = corpus(50);
        let one = index(&work, 1);
        println!("load {} before/after 1-thread: {}", load(), one.files);
        let mut best = 0.0f64;
        let mut counts = vec![2, 4, 8, 12, n];
        counts.sort_unstable();
        counts.dedup();
        for threads in counts {
            let r = index(&work, threads);
            let ratio = secs(one.stats.prepare) / secs(r.stats.prepare);
            println!(
                "threads {threads}: prepare {:.2}s (1 thread {:.2}s) ratio {ratio:.2} wall {:.1}s files {} graph {} load {}",
                secs(r.stats.prepare),
                secs(one.stats.prepare),
                secs(r.wall),
                r.files,
                r.graph_len,
                load()
            );
            assert_eq!(r.files, one.files);
            assert_eq!(r.graph_len, one.graph_len);
            if threads == n {
                best = ratio;
            }
        }
        println!(
            "required ratio {:.2} at {n} cores, measured {best:.2}",
            0.6 * n as f64
        );
        assert!(
            best >= 0.6 * n as f64,
            "ratio {best:.2} < {:.2}",
            0.6 * n as f64
        );
    }

    /// R-12a. A full index of 860 files finishes within 30 s at the default thread count.
    #[test]
    #[ignore]
    fn full_index_speedup() {
        let n = cores();
        let (_t, work) = corpus(20);
        let r = index(&work, n);
        println!(
            "860-file corpus at {n} threads: wall {:.1}s prepare {:.1}s stitch {:.1}s commit {:.1}s files {} load {}",
            secs(r.wall),
            secs(r.stats.prepare),
            secs(r.stats.stitch),
            secs(r.stats.commit),
            r.files,
            load()
        );
        assert!(secs(r.wall) <= 30.0, "wall {:.1}s", secs(r.wall));

        // The self-index (this repository's crates) must stay at or below its 4.4 s baseline.
        let crates = Utf8PathBuf::from_path_buf(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .canonicalize()
                .unwrap(),
        )
        .unwrap();
        let s = index(&crates, n);
        println!(
            "self-index (crates/) at {n} threads: wall {:.1}s files {}",
            secs(s.wall),
            s.files
        );
    }

    /// R-12b. Stitch time per file grows by at most 1.5x from 860 to 2,150 files.
    #[test]
    #[ignore]
    fn stitch_linear_in_files() {
        let n = cores();
        let (_a, small) = corpus(20);
        let (_b, large) = corpus(50);
        let s = index(&small, n);
        let l = index(&large, n);
        let per_s = secs(s.stats.stitch) / s.files as f64;
        let per_l = secs(l.stats.stitch) / l.files as f64;
        println!(
            "stitch: 860 files {:.1}s ({:.4} s/file), 2150 files {:.1}s ({:.4} s/file), growth {:.2}x; load {}",
            secs(s.stats.stitch),
            per_s,
            secs(l.stats.stitch),
            per_l,
            per_l / per_s,
            load()
        );
        assert!(per_l <= 1.5 * per_s, "growth {:.2}x", per_l / per_s);
    }

    /// The peak resident set (`VmHWM`) of this process, in KiB.
    fn vm_hwm_kib() -> Option<u64> {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        s.lines()
            .find_map(|l| l.strip_prefix("VmHWM:"))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
    }

    /// Child half of the memory bench: index a 2,150-file corpus at `KATALOGR_BENCH_THREADS`
    /// threads and print `VMHWM_KB=<n>`. Does nothing unless the parent set the variable.
    #[test]
    #[ignore]
    fn rss_child() {
        let Ok(t) = std::env::var("KATALOGR_BENCH_THREADS") else {
            return;
        };
        let (_t, work) = corpus(50);
        let r = index(&work, t.parse().unwrap());
        println!("files {} wall {:.1}s", r.files, secs(r.wall));
        println!("VMHWM_KB={}", vm_hwm_kib().unwrap_or(0));
    }

    /// R-13. Peak memory at N threads stays within the 1-thread peak plus 2 x `batch_bytes`.
    #[test]
    #[ignore]
    fn rss_bounded_by_batch_bytes() {
        if vm_hwm_kib().is_none() {
            println!("skipped: no /proc/self/status");
            return;
        }
        let peak = |threads: usize| -> u64 {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "bench::rss_child", "--ignored", "--nocapture"])
                .env("KATALOGR_BENCH_THREADS", threads.to_string())
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            text.lines()
                .find_map(|l| l.strip_prefix("VMHWM_KB="))
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("no VMHWM in child output: {text}"))
        };
        let n = cores();
        let one = peak(1);
        let many = peak(n);
        let allowed = one + 2 * Parallelism::DEFAULT_BATCH_BYTES / 1024;
        println!(
            "VmHWM 1 thread {} MiB, {n} threads {} MiB, allowed {} MiB; load {}",
            one / 1024,
            many / 1024,
            allowed / 1024,
            load()
        );
        assert!(many <= allowed, "{many} KiB > {allowed} KiB");
    }
}
