//! Evaluation benchmarks (FEAT-061 R-11). Ignored; run in release:
//! `cargo test -p katalogr-dist --release --test bench -- --ignored --nocapture`.
//!
//! The context benchmarks (FEAT-043 R-13, R-14) build their corpus under
//! `~/.cache/katalogr-scratch/FEAT-043/` (or `KATALOGR_SCRATCH`), never `/tmp`, and remove it.
//! Run them one at a time, under the shared heavy-job lock:
//! `flock ~/.cache/heavy-build.lock cargo test -p katalogr-dist --release --test bench
//! context_ -- --ignored --nocapture --test-threads=1`.

#[path = "support/synth.rs"]
mod synth;

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

    /// FEAT-084 R-12: computing a view ID takes under 100 microseconds.
    #[test]
    #[ignore]
    fn view_id_cost() {
        use katalogr_query::{PositionRef, ViewQuery, view_id};
        let positions: Vec<PositionRef> = (0..3)
            .map(|i| PositionRef {
                repo: format!("repo{i}"),
                commit: Some("e410760849abb8cd9fa8fbba305f5dd5d96ba954".to_string()),
                dirty: Some(
                    "83ebb47e03fee94773edcd621a87f37980b3b408b96467062fcc87368f3260c7".to_string(),
                ),
            })
            .collect();
        let q = ViewQuery {
            op: "search".to_string(),
            args: [("terms", "rate code"), ("limit", "20"), ("budget", "400")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        let facts = "0123456789abcdef".repeat(4);
        let n = 20_000;
        let mut sink = 0usize;
        let t = Instant::now();
        for _ in 0..n {
            sink += view_id(
                std::hint::black_box(&positions),
                &facts,
                std::hint::black_box(&q),
            )
            .len();
        }
        let us = t.elapsed().as_secs_f64() * 1e6 / n as f64;
        println!(
            "view_id_cost: {us:.2} us per ID (sink {sink}); load {}",
            load().trim()
        );
        assert!(us < 100.0, "{us:.2} us is over the 100 us budget");
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

    /// FEAT-039 R-12: an idle `watch` tick on a 10k-file repo costs under 5% of one core at the
    /// default 2 s interval, that is under 100 ms of CPU per tick. Ticks run here every 100 ms
    /// so the run is short; the cost per tick is what is compared with the budget. Not run by
    /// the author: run it in release, alone, under the heavy-job lock:
    /// `flock ~/.cache/heavy-build.lock cargo test -p katalogr-dist --release --test bench
    /// watch_tick_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn watch_tick_cost() {
        use katalogr_dist::watch::{WatchTick, watch};
        use katalogr_dist::{Katalogr, Overrides, kindex_init, project_init};
        use std::sync::atomic::{AtomicBool, Ordering};

        let base = std::env::var("KATALOGR_SCRATCH")
            .unwrap_or_else(|_| "/home/dev/.cache/katalogr-scratch/FEAT-039".to_string());
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(&base).unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let repo = root.join("app");
        // Files no extension reads, so the first tick has nothing to extract.
        for d in 0..100 {
            let sub = repo.join(format!("d{d:03}"));
            std::fs::create_dir_all(&sub).unwrap();
            for f in 0..100 {
                std::fs::write(sub.join(format!("f{f:03}.dat")), format!("file {d} {f}\n"))
                    .unwrap();
            }
        }
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .current_dir(&repo)
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
        let kx = root.join("kx");
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        let k = Katalogr::resolve(
            &repo,
            &Overrides {
                kindex: Some(kx.display().to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        // Files younger than the stat cache by under 2 s are read again (the racy rule).
        std::thread::sleep(Duration::from_secs(3));

        let stop = AtomicBool::new(false);
        let load_before = load();
        let (mut idle, mut cpu_start, mut wall_start) = (0u32, 0.0, None);
        let mut cpu_total = 0.0;
        let mut wall_total = 0.0;
        watch(&k, Duration::from_millis(100), &stop, &mut |t| match t {
            WatchTick::Idle => {
                if idle == 0 {
                    cpu_start = cpu_secs();
                    wall_start = Some(Instant::now());
                }
                idle += 1;
                if idle == 31 {
                    cpu_total = cpu_secs() - cpu_start;
                    wall_total = wall_start.unwrap().elapsed().as_secs_f64();
                    stop.store(true, Ordering::Relaxed);
                }
            }
            WatchTick::Processed { .. } => {}
            WatchTick::Failed(e) => panic!("tick failed: {e}"),
        })
        .unwrap();
        // 30 intervals separate the first idle tick from the 31st; each interval holds one tick
        // and 100 ms of sleep, which costs no CPU.
        let per_tick = cpu_total / 30.0;
        let core_pct = per_tick / 2.0 * 100.0;
        println!(
            "watch_tick_cost: {:.1} ms CPU per idle tick ({:.2}% of one core at 2 s); {wall_total:.2} s wall for 30 ticks; load {load_before} -> {}",
            per_tick * 1e3,
            core_pct,
            load()
        );
        assert!(core_pct < 5.0, "{core_pct:.2}% of a core exceeds 5%");
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

    // ---- contexts (FEAT-043) ----------------------------------------------------------

    use camino::Utf8PathBuf;
    use katalogr_engine::parallel::Parallelism;
    use katalogr_engine::registry::ExtensionRegistry;
    use katalogr_engine::{
        Consistency, Engine, EventKind, NewEvent, QueryOptions, Scope, canonical_graph,
    };
    use katalogr_model::RelPath;
    use katalogr_store::{
        ContextId, ContextKind, ContextManifest, Kindex, ProjectConfig, ProjectDir, ProjectName,
        RepoConfig, seed_layer,
    };
    use std::collections::BTreeMap;
    use std::num::NonZeroUsize;
    use std::time::Duration;

    fn load() -> String {
        std::fs::read_to_string("/proc/loadavg")
            .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    }

    /// A scratch directory on disk, outside `/tmp`, removed when dropped.
    fn scratch() -> tempfile::TempDir {
        let base = std::env::var("KATALOGR_SCRATCH").map_or_else(
            |_| {
                let home = std::env::var("HOME").expect("HOME");
                std::path::PathBuf::from(home).join(".cache/katalogr-scratch/FEAT-043")
            },
            std::path::PathBuf::from,
        );
        std::fs::create_dir_all(&base).unwrap();
        tempfile::Builder::new()
            .prefix("ctx-")
            .tempdir_in(base)
            .unwrap()
    }

    fn registry() -> ExtensionRegistry {
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(katalogr_ext_rust::RustExtractor));
        reg.register(Box::new(katalogr_ext_md::MarkdownExtractor));
        reg
    }

    fn open(dir: ProjectDir, work: &Utf8PathBuf) -> Engine {
        let roots = BTreeMap::from([("r".to_string(), work.clone())]);
        let mut e = Engine::open(
            dir,
            registry(),
            roots,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        e.set_parallelism(Parallelism {
            threads: NonZeroUsize::new(4).unwrap(),
            ..Parallelism::sequential()
        });
        e
    }

    fn wait_all(e: &Engine) -> usize {
        let o = QueryOptions {
            consistency: Consistency::WaitAll,
            timeout: Duration::from_secs(3600),
            scope: Scope::Project,
        };
        let a = e.with_consistency(&o, |tx| canonical_graph(tx)).unwrap();
        assert!(!a.stale);
        a.value.len()
    }

    /// A base indexed from a generated corpus of `crates` x `modules` x `fns`.
    fn indexed_base(
        tmp: &Path,
        crates: usize,
        modules: usize,
        fns: usize,
    ) -> (Utf8PathBuf, Utf8PathBuf) {
        let root = Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let (kx, work) = (root.join("kx"), root.join("work"));
        super::synth::rust_corpus(work.as_std_path(), crates, modules, fns);
        let base = Kindex::init(&kx)
            .unwrap()
            .create_project(
                &ProjectName::new("p").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let e = open(base, &work);
        e.inbox().append(&NewEvent::rescan("r")).unwrap();
        wait_all(&e);
        (kx, work)
    }

    fn base_of(kx: &Utf8PathBuf) -> ProjectDir {
        Kindex::open(kx)
            .unwrap()
            .project(&ProjectName::new("p").unwrap())
            .unwrap()
    }

    fn manifest(id: &ContextId) -> ContextManifest {
        ContextManifest {
            format: 1,
            id: id.clone(),
            kind: ContextKind::Checkout,
            repo: "r".into(),
            root: None,
            seed_commit: None,
            commit: None,
        }
    }

    fn dir_bytes(p: &std::path::Path) -> u64 {
        std::fs::read_dir(p).map_or(0, |rd| {
            rd.flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => dir_bytes(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
    }

    /// R-13: one file saved in a context to the next fresh read, on a fixture of about 10,000
    /// symbols (10 crates of 40 modules of 20 functions). At most 500 ms by the median.
    #[test]
    #[ignore]
    fn context_save_to_answer() {
        let tmp = scratch();
        let (kx, work) = indexed_base(tmp.path(), 10, 40, 20);
        let base = base_of(&kx);
        let id = ContextId::new("w-bench").unwrap();
        let layer = base.layer(&id);
        seed_layer(
            &base,
            &layer,
            &manifest(&id),
            Instant::now() + Duration::from_secs(120),
        )
        .unwrap();
        let e = open(layer, &work);
        e.inbox().append(&NewEvent::rescan("r")).unwrap();
        wait_all(&e);

        let load_before = load();
        let mut ms = Vec::new();
        for n in 0..21u32 {
            let rel = format!("c3/src/m{}.rs", n % 40);
            let file = work.join(&rel);
            let mut text = std::fs::read_to_string(&file).unwrap();
            text.push_str(&format!(
                "\npub fn edited_{n}(x: u32) -> u32 {{ x + {n} }}\n"
            ));
            std::fs::write(&file, text).unwrap();
            let t = Instant::now();
            e.inbox()
                .append(&NewEvent::file(
                    "r",
                    &RelPath::new(&rel).unwrap(),
                    EventKind::Changed,
                ))
                .unwrap();
            wait_all(&e);
            // The first run warms caches and is not counted.
            if n > 0 {
                ms.push(t.elapsed().as_millis());
            }
        }
        ms.sort_unstable();
        let (median, p95) = (ms[ms.len() / 2], ms[ms.len() * 95 / 100]);
        println!(
            "context_save_to_answer: median {median} ms, p95 {p95} ms over {} saves; load {load_before} -> {}",
            ms.len(),
            load()
        );
        assert!(median <= 500, "median {median} ms exceeds 500 ms");
    }

    /// R-14: seeding a context, as seconds per GiB of the base's cache on a local SSD; records
    /// the disk the layer uses. At most 5 s per GiB.
    #[test]
    #[ignore]
    fn context_seed_per_gib() {
        let tmp = scratch();
        let (kx, _work) = indexed_base(tmp.path(), 20, 40, 20);
        let base = base_of(&kx);
        let cache = std::fs::metadata(base.cache_dir().join("index.redb"))
            .unwrap()
            .len();
        let id = ContextId::new("w-bench").unwrap();
        let layer = base.layer(&id);
        let load_before = load();
        let t = Instant::now();
        seed_layer(
            &base,
            &layer,
            &manifest(&id),
            Instant::now() + Duration::from_secs(600),
        )
        .unwrap();
        let secs = t.elapsed().as_secs_f64();
        let gib = cache as f64 / (1u64 << 30) as f64;
        let per_gib = secs / gib;
        println!(
            "context_seed_per_gib: {secs:.3} s for a {:.1} MiB cache = {per_gib:.2} s/GiB; layer uses {} bytes; load {load_before} -> {}",
            cache as f64 / (1u64 << 20) as f64,
            dir_bytes(layer.root().as_std_path()),
            load()
        );
        assert!(per_gib <= 5.0, "{per_gib:.2} s/GiB exceeds 5 s/GiB");
    }

    /// FEAT-085 R-5: seeding a second worktree at the base commit extracts nothing.
    #[cfg(feature = "test-extractor")]
    #[test]
    #[ignore]
    fn second_worktree_seed_extracts_nothing() {
        use katalogr_dist::{
            Katalogr, Options, Overrides, extract_calls, kindex_init, project_init,
        };
        let git = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
        };
        let tmp = scratch();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let main = root.join("app");
        let wt = root.join("app-wt");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        for i in 0..200 {
            std::fs::write(main.join(format!("f{i}.tx")), format!("sym fn gen{i}\n")).unwrap();
        }
        std::fs::write(main.join(".gitignore"), ".katalogr\n").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-q", "-m", "base"]);
        git(
            &main,
            &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
        );
        let kx = root.join("kx");
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), main.clone())], true).unwrap();

        let km = Katalogr::resolve(&main, &Overrides::default()).unwrap();
        let before = extract_calls();
        km.index(None, false, None, &Options::default()).unwrap();
        let base_calls = extract_calls() - before;

        let load_before = load();
        let kw = Katalogr::resolve(&wt, &Overrides::default()).unwrap();
        let before = extract_calls();
        let t = Instant::now();
        kw.index(None, false, None, &Options::default()).unwrap();
        let secs = t.elapsed().as_secs_f64();
        let seed_calls = extract_calls() - before;
        println!(
            "second_worktree_seed_extracts_nothing: base extracted {base_calls} files; second worktree {seed_calls} files in {secs:.3} s; load {load_before} -> {}",
            load()
        );
        assert!(base_calls >= 200, "{base_calls}");
        assert_eq!(seed_calls, 0);
    }
}
