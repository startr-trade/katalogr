//! FEAT-093 R-10 and R-11: a full index of a real corpus stays inside its wall, memory and size
//! budget. The corpora and budgets come from the environment, so no corpus name or path is kept
//! in this repository, and the test is ignored: run it by hand, one corpus at a time, with
//! `TMPDIR` on disk and `systemd-run --user --scope -p MemoryMax=8G`.
//!
//! ```text
//! KATALOGR_BIN=<release binary> \
//! KATALOGR_BUDGET_REPOS=name=/path/to/clone[,name2=/path2] \
//! KATALOGR_BUDGET=wall_s=360,rss_mib=3600,index_mib=1500,files=12541 \
//!   cargo test -p katalogr-bench --release --test budget -- --ignored --nocapture
//! ```
//!
//! FEAT-076 R-9 to R-11: `corpus_query_budget` times the read commands on an index it builds
//! (or on one you name), against limits from the environment:
//!
//! ```text
//! KATALOGR_BIN=<release binary> \
//! KATALOGR_BUDGET_REPOS=name=/path/to/clone[,name2=/path2] \
//! KATALOGR_QUERY_TARGET=<canonical name of a symbol that has references> \
//! KATALOGR_QUERY_BUDGET=query_ms=500,query_mib=200,session_ms=500,session_mib=64,map_ms=2500,insight_ms=2500,insight_mib=1300 \
//!   cargo test -p katalogr-bench --release --test budget corpus_query_budget -- --ignored --nocapture
//! ```
//!
//! FEAT-080 R-12: `session-start` has a fixed line of 100 ms (`SESSION_START_MS`) on top of the
//! environment's `session_ms`; the smaller applies.
//!
//! `<command>_ms` (`hubs_ms`, `communities_ms`, `cycles_ms`, `dead-code_ms`, `coverage_ms`,
//! `map_ms`) overrides `insight_ms` for one command. `KATALOGR_QUERY_KINDEX` and
//! `KATALOGR_QUERY_PROJECT` name an index that is already built and is only read.
//! `KATALOGR_QUERY_REPS` (default 5) is the number of runs per command.
//!
//! FEAT-080 R-10 and R-11: `concurrent_query_budget` indexes the corpus twice, once alone and
//! once with `KATALOGR_CONCURRENT_WORKERS` (default 4) `stale-ok` query loops started after the
//! first round has published. It fails on any query error, on a p95 over its line, on a slowdown
//! over the limit, or on shards that differ:
//!
//! ```text
//! KATALOGR_BIN=<release binary> \
//! KATALOGR_BUDGET_REPOS=name=/path/to/clone[,name2=/path2] \
//! KATALOGR_QUERY_TARGET=<canonical name of a symbol that has references> \
//! KATALOGR_QUERY_TERM=<a word the corpus contains> \
//! KATALOGR_CONCURRENT_BUDGET=find_ms=500,search_ms=1000,session_ms=1000,slowdown_pct=115 \
//!   cargo test -p katalogr-bench --release --test budget concurrent_query_budget -- --ignored --nocapture
//! ```

mod bench {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_bench::katalogr::Katalogr;
    use katalogr_bench::proc::run_with_timeout;
    use katalogr_bench::sizes::{cache_bytes, shard_stats};
    use katalogr_bench::stats::nearest_rank;
    use std::collections::BTreeMap;
    use std::process::Command;
    use std::time::Duration;

    const PROJECT: &str = "budget";
    /// FEAT-080 R-12: the line for `session-start` on a full cache.
    const SESSION_START_MS: f64 = 100.0;
    const MIB: f64 = 1024.0 * 1024.0;

    fn env(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| {
            panic!(
                "{name} is not set: this test needs KATALOGR_BIN, KATALOGR_BUDGET_REPOS \
                 (name=path,...) and KATALOGR_BUDGET (wall_s=..,rss_mib=..,index_mib=..,files=..)"
            )
        })
    }

    fn budget(spec: &str) -> BTreeMap<String, u64> {
        let map: BTreeMap<String, u64> = spec
            .split(',')
            .map(|kv| {
                let (k, v) = kv
                    .split_once('=')
                    .unwrap_or_else(|| panic!("KATALOGR_BUDGET: bad entry `{kv}`"));
                let v = v
                    .trim()
                    .parse()
                    .unwrap_or_else(|_| panic!("KATALOGR_BUDGET: `{kv}` is not a number"));
                (k.trim().to_string(), v)
            })
            .collect();
        for k in ["wall_s", "rss_mib", "index_mib", "files"] {
            assert!(map.contains_key(k), "KATALOGR_BUDGET lacks `{k}`");
        }
        map
    }

    fn load_average() -> String {
        std::fs::read_to_string("/proc/loadavg")
            .map(|s| s.split_whitespace().next().unwrap_or("?").to_string())
            .unwrap_or_else(|_| "?".into())
    }

    fn ok(mut cmd: Command, what: &str) {
        let out = cmd
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", std::env::var_os("HOME").unwrap_or_default())
            .output()
            .unwrap_or_else(|e| panic!("{what}: cannot start: {e}"));
        assert!(
            out.status.success(),
            "{what}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    #[ignore = "benchmark: needs a corpus; see the module comment"]
    fn corpus_full_index_budget() {
        let bin = Utf8PathBuf::from(env("KATALOGR_BIN"));
        let repos = env("KATALOGR_BUDGET_REPOS");
        let want = budget(&env("KATALOGR_BUDGET"));
        let tmp = tempfile::tempdir().expect("temporary kindex");
        let kindex = Utf8Path::from_path(tmp.path())
            .expect("UTF-8 temporary directory")
            .join("kx");

        let mut init = Command::new(&bin);
        init.args([
            "--kindex",
            kindex.as_str(),
            "kindex",
            "init",
            kindex.as_str(),
        ]);
        ok(init, "kindex init");
        let mut project = Command::new(&bin);
        project.args(["--kindex", kindex.as_str(), "project", "init", PROJECT]);
        for r in repos.split(',') {
            project.args(["--repo", r]);
        }
        project.arg("--mark");
        ok(project, "project init");

        let k = Katalogr {
            bin: bin.clone(),
            kindex: kindex.clone(),
            project: PROJECT.into(),
        };
        let load_before = load_average();
        let mut index = k.cmd(&["index", "--full"]);
        index.env("KATALOGR_THREADS", "4");
        let limit = Duration::from_secs(want["wall_s"] * 2);
        let run = run_with_timeout(&mut index, limit).expect("run index --full");
        let load_after = load_average();
        assert!(
            !run.timed_out,
            "index --full ran past twice its wall budget ({limit:?}); load {load_before} to {load_after}"
        );
        assert_eq!(
            run.status,
            Some(0),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );

        let project_dir = kindex.join("projects").join(PROJECT);
        let shards = shard_stats(&project_dir).expect("shard sizes");
        let cache = cache_bytes(&project_dir).expect("cache size");
        let index_mib = (shards.bytes + cache) as f64 / MIB;
        let rss_mib = run.peak_rss_kib.expect("peak RSS (Linux)") as f64 / 1024.0;
        let wall_s = run.wall.as_secs_f64();

        let mut cov = k.cmd(&["coverage", "--suffixes", "5"]);
        cov.env("KATALOGR_THREADS", "4");
        let cov = run_with_timeout(&mut cov, Duration::from_secs(600)).expect("run coverage");
        assert_eq!(cov.status, Some(0));
        let json: serde_json::Value = serde_json::from_slice(&cov.stdout).expect("coverage JSON");
        let files = json
            .pointer("/result/totals/files_indexed")
            .and_then(serde_json::Value::as_u64)
            .expect("files_indexed in coverage");

        println!(
            "load {load_before} to {load_after}: wall {wall_s:.1} s (budget {}), peak RSS {rss_mib:.0} MiB (budget {}), \
             index {index_mib:.0} MiB = shards {:.0} + cache {:.0} (budget {}), claimed files {files} (expected {})",
            want["wall_s"],
            want["rss_mib"],
            shards.bytes as f64 / MIB,
            cache as f64 / MIB,
            want["index_mib"],
            want["files"],
        );
        assert!(wall_s <= want["wall_s"] as f64, "wall {wall_s:.1} s");
        assert!(
            rss_mib <= want["rss_mib"] as f64,
            "peak RSS {rss_mib:.0} MiB"
        );
        assert!(
            index_mib <= want["index_mib"] as f64,
            "index {index_mib:.0} MiB"
        );
        assert_eq!(files, want["files"], "claimed files");
    }

    /// The limits of one command group: `<group>_ms` (p95 wall) and `<group>_mib` (peak RSS).
    fn limits(spec: &BTreeMap<String, u64>, group: &str, command: &str) -> (f64, f64) {
        let ms = spec
            .get(&format!("{command}_ms"))
            .or_else(|| spec.get(&format!("{group}_ms")))
            .unwrap_or_else(|| panic!("KATALOGR_QUERY_BUDGET lacks `{group}_ms`"));
        let mib = spec
            .get(&format!("{command}_mib"))
            .or_else(|| spec.get(&format!("{group}_mib")))
            .unwrap_or_else(|| panic!("KATALOGR_QUERY_BUDGET lacks `{group}_mib`"));
        (*ms as f64, *mib as f64)
    }

    #[test]
    #[ignore = "benchmark: needs a corpus; see the module comment"]
    fn corpus_query_budget() {
        let bin = Utf8PathBuf::from(env("KATALOGR_BIN"));
        let spec: BTreeMap<String, u64> = env("KATALOGR_QUERY_BUDGET")
            .split(',')
            .map(|kv| {
                let (k, v) = kv
                    .split_once('=')
                    .unwrap_or_else(|| panic!("KATALOGR_QUERY_BUDGET: bad entry `{kv}`"));
                (
                    k.trim().to_string(),
                    v.trim().parse().unwrap_or_else(|_| {
                        panic!("KATALOGR_QUERY_BUDGET: `{kv}` is not a number")
                    }),
                )
            })
            .collect();
        let target = env("KATALOGR_QUERY_TARGET");
        let reps: usize = std::env::var("KATALOGR_QUERY_REPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);
        let _keep;
        let k = match (
            std::env::var("KATALOGR_QUERY_KINDEX"),
            std::env::var("KATALOGR_QUERY_PROJECT"),
        ) {
            (Ok(kindex), Ok(project)) => Katalogr {
                bin: bin.clone(),
                kindex: Utf8PathBuf::from(kindex),
                project,
            },
            _ => {
                let repos = env("KATALOGR_BUDGET_REPOS");
                let tmp = tempfile::tempdir().expect("temporary kindex");
                let kindex = Utf8Path::from_path(tmp.path())
                    .expect("UTF-8 temporary directory")
                    .join("kx");
                _keep = tmp;
                let mut init = Command::new(&bin);
                init.args([
                    "--kindex",
                    kindex.as_str(),
                    "kindex",
                    "init",
                    kindex.as_str(),
                ]);
                ok(init, "kindex init");
                let mut project = Command::new(&bin);
                project.args(["--kindex", kindex.as_str(), "project", "init", PROJECT]);
                for r in repos.split(',') {
                    project.args(["--repo", r]);
                }
                project.arg("--mark");
                ok(project, "project init");
                let k = Katalogr {
                    bin: bin.clone(),
                    kindex,
                    project: PROJECT.into(),
                };
                let mut index = k.cmd(&["index", "--full"]);
                index.env("KATALOGR_THREADS", "4");
                let run = run_with_timeout(&mut index, Duration::from_secs(7200))
                    .expect("run index --full");
                assert_eq!(
                    run.status,
                    Some(0),
                    "{}",
                    String::from_utf8_lossy(&run.stderr)
                );
                k
            }
        };

        // (group, command, arguments)
        let t = target.as_str();
        let plan: Vec<(&str, &str, Vec<&str>)> = vec![
            ("query", "find", vec!["find", "no_such_name_anywhere_zq"]),
            ("query", "refs", vec!["refs", t]),
            ("query", "callers", vec!["callers", t]),
            ("query", "explain", vec!["explain", t]),
            ("query", "tests", vec!["tests", t]),
            ("session", "session-start", vec!["session-start"]),
            ("map", "map", vec!["map"]),
            ("insight", "hubs", vec!["hubs"]),
            ("insight", "communities", vec!["communities"]),
            ("insight", "cycles", vec!["cycles"]),
            ("insight", "dead-code", vec!["dead-code"]),
            ("insight", "coverage", vec!["coverage"]),
        ];
        let mut failures = Vec::new();
        for (group, command, args) in &plan {
            let (mut ms_limit, mib_limit) = limits(&spec, group, command);
            if *command == "session-start" {
                ms_limit = ms_limit.min(SESSION_START_MS);
            }
            let (mut walls, mut peak) = (Vec::new(), 0.0f64);
            let load = load_average();
            for _ in 0..reps {
                let mut cmd = k.cmd(args);
                cmd.env("KATALOGR_THREADS", "4");
                let run = run_with_timeout(&mut cmd, Duration::from_secs(600)).expect("run");
                // `find` of a name nothing has answers "no match" (exit 4): a correct answer.
                let expected: &[Option<i32>] = if *command == "find" {
                    &[Some(0), Some(4)]
                } else {
                    &[Some(0)]
                };
                assert!(
                    expected.contains(&run.status),
                    "{command}: status {:?}: {}",
                    run.status,
                    String::from_utf8_lossy(&run.stderr)
                );
                walls.push(run.wall.as_secs_f64() * 1000.0);
                peak = peak.max(run.peak_rss_kib.expect("peak RSS (Linux)") as f64 / 1024.0);
            }
            walls.sort_by(|a, b| a.total_cmp(b));
            let p95 = nearest_rank(&walls, 95.0);
            println!(
                "load {load} to {}: {command}: p50 {:.0} ms, p95 {p95:.0} ms (limit {ms_limit}), peak RSS {peak:.0} MiB (limit {mib_limit}), {reps} runs",
                load_average(),
                walls[walls.len() / 2],
            );
            if p95 > ms_limit {
                failures.push(format!("{command}: p95 {p95:.0} ms over {ms_limit}"));
            }
            if peak > mib_limit {
                failures.push(format!(
                    "{command}: peak RSS {peak:.0} MiB over {mib_limit}"
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("; "));
    }

    /// A fresh kindex with the project over `repos`.
    fn new_project(bin: &Utf8Path, root: &Utf8Path, name: &str, repos: &str) -> Katalogr {
        let kindex = root.join(name);
        let mut init = Command::new(bin);
        init.args([
            "--kindex",
            kindex.as_str(),
            "kindex",
            "init",
            kindex.as_str(),
        ]);
        ok(init, "kindex init");
        let mut project = Command::new(bin);
        project.args(["--kindex", kindex.as_str(), "project", "init", PROJECT]);
        for r in repos.split(',') {
            project.args(["--repo", r]);
        }
        project.arg("--mark");
        ok(project, "project init");
        Katalogr {
            bin: bin.to_owned(),
            kindex,
            project: PROJECT.into(),
        }
    }

    /// FEAT-080 R-10 and R-11.
    #[test]
    #[ignore = "benchmark: needs a corpus; see the module comment"]
    fn concurrent_query_budget() {
        use katalogr_bench::proc::spawn_with;
        use katalogr_bench::sizes::{shard_digest, shard_stats};
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicBool, Ordering};

        let bin = Utf8PathBuf::from(env("KATALOGR_BIN"));
        let repos = env("KATALOGR_BUDGET_REPOS");
        let target = env("KATALOGR_QUERY_TARGET");
        let term = env("KATALOGR_QUERY_TERM");
        let spec: BTreeMap<String, u64> = env("KATALOGR_CONCURRENT_BUDGET")
            .split(',')
            .map(|kv| {
                let (k, v) = kv
                    .split_once('=')
                    .unwrap_or_else(|| panic!("KATALOGR_CONCURRENT_BUDGET: bad entry `{kv}`"));
                (
                    k.trim().to_string(),
                    v.trim().parse().unwrap_or_else(|_| {
                        panic!("KATALOGR_CONCURRENT_BUDGET: `{kv}` is not a number")
                    }),
                )
            })
            .collect();
        for k in ["find_ms", "search_ms", "session_ms", "slowdown_pct"] {
            assert!(
                spec.contains_key(k),
                "KATALOGR_CONCURRENT_BUDGET lacks `{k}`"
            );
        }
        let workers: usize = std::env::var("KATALOGR_CONCURRENT_WORKERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        let tmp = tempfile::tempdir().expect("temporary kindex");
        let root = Utf8Path::from_path(tmp.path()).expect("UTF-8 temporary directory");

        // Alone first: the wall and the shards to compare with.
        let cold = new_project(&bin, root, "cold", &repos);
        let mut index = cold.cmd(&["index", "--full"]);
        index.env("KATALOGR_THREADS", "4");
        let load_cold = load_average();
        let alone = run_with_timeout(&mut index, Duration::from_secs(7200)).expect("cold index");
        assert_eq!(
            alone.status,
            Some(0),
            "{}",
            String::from_utf8_lossy(&alone.stderr)
        );
        let cold_digest = shard_digest(&cold.kindex.join("projects").join(PROJECT)).unwrap();

        // Then with workers, started once the first round has published.
        let busy = new_project(&bin, root, "busy", &repos);
        let busy_dir = busy.kindex.join("projects").join(PROJECT);
        let stop = AtomicBool::new(false);
        let seen: Mutex<BTreeMap<&'static str, (Vec<f64>, u32)>> = Mutex::new(BTreeMap::new());
        let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let ops: Vec<(&'static str, Vec<String>)> = vec![
            ("find", vec!["find".into(), target.clone()]),
            ("find", vec!["refs".into(), target.clone()]),
            ("find", vec!["callers".into(), target.clone()]),
            ("find", vec!["explain".into(), target.clone()]),
            ("search", vec!["search".into(), term.clone()]),
            ("session", vec!["session-start".into()]),
        ];
        let load_busy = load_average();
        let mut index = busy.cmd(&["index", "--full"]);
        index.env("KATALOGR_THREADS", "4");
        let together = std::thread::scope(|scope| {
            let running = spawn_with(&mut index, Some(Duration::from_secs(7200))).expect("spawn");
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(600) {
                let cache = busy_dir.join(".cache").join("index.redb").exists();
                if cache && shard_stats(&busy_dir).map_or(0, |s| s.files) > 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let handles: Vec<_> = (0..workers)
                .map(|id| {
                    let (busy, ops, stop, seen, errors) = (&busy, &ops, &stop, &seen, &errors);
                    scope.spawn(move || {
                        let mut i = id;
                        while !stop.load(Ordering::Relaxed) {
                            let (group, args) = &ops[i % ops.len()];
                            i += 1;
                            let mut full: Vec<&str> = vec!["--consistency", "stale-ok"];
                            full.extend(args.iter().map(String::as_str));
                            let mut cmd = busy.cmd(&full);
                            let r = run_with_timeout(&mut cmd, Duration::from_secs(120));
                            match r {
                                // Exit 4: before its file is indexed an op can match nothing.
                                Ok(r) if matches!(r.status, Some(0) | Some(4)) => {
                                    let mut g = seen.lock().unwrap();
                                    let e = g.entry(group).or_default();
                                    e.0.push(r.wall.as_secs_f64() * 1000.0);
                                    e.1 += 1;
                                }
                                Ok(r) => errors.lock().unwrap().push(format!(
                                    "{}: exit {:?}: {}",
                                    args[0],
                                    r.status,
                                    String::from_utf8_lossy(&r.stderr)
                                        .lines()
                                        .next()
                                        .unwrap_or("")
                                )),
                                Err(e) => errors.lock().unwrap().push(format!("{}: {e}", args[0])),
                            }
                        }
                    })
                })
                .collect();
            let done = running.finish().expect("concurrent index");
            stop.store(true, Ordering::Relaxed);
            for h in handles {
                let _ = h.join();
            }
            done
        });
        assert_eq!(
            together.status,
            Some(0),
            "{}",
            String::from_utf8_lossy(&together.stderr)
        );
        let busy_digest = shard_digest(&busy_dir).unwrap();

        let slowdown = together.wall.as_secs_f64() / alone.wall.as_secs_f64();
        println!(
            "load {load_cold} (alone) / {load_busy} (with workers) to {}: index alone {:.1} s, with {workers} workers {:.1} s = {slowdown:.2}x (limit {:.2}x)",
            load_average(),
            alone.wall.as_secs_f64(),
            together.wall.as_secs_f64(),
            spec["slowdown_pct"] as f64 / 100.0,
        );
        let mut failures = Vec::new();
        let errors = errors.into_inner().unwrap();
        if !errors.is_empty() {
            failures.push(format!(
                "{} query errors, first: {}",
                errors.len(),
                errors[0]
            ));
        }
        for (group, (mut ms, n)) in seen.into_inner().unwrap() {
            ms.sort_by(|a, b| a.total_cmp(b));
            let p95 = nearest_rank(&ms, 95.0);
            let limit = spec[&format!("{group}_ms")] as f64;
            println!(
                "{group}: {n} queries, p50 {:.0} ms, p95 {p95:.0} ms (limit {limit})",
                ms[ms.len() / 2]
            );
            if p95 > limit {
                failures.push(format!("{group}: p95 {p95:.0} ms over {limit}"));
            }
        }
        if slowdown * 100.0 > spec["slowdown_pct"] as f64 {
            failures.push(format!("index slowdown {slowdown:.2}x"));
        }
        if busy_digest != cold_digest {
            failures.push("shards differ from the index run alone".into());
        }
        assert!(failures.is_empty(), "{}", failures.join("; "));
    }
}
