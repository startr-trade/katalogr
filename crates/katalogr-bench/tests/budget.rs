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
//! `<command>_ms` (`hubs_ms`, `communities_ms`, `cycles_ms`, `dead-code_ms`, `coverage_ms`,
//! `map_ms`) overrides `insight_ms` for one command. `KATALOGR_QUERY_KINDEX` and
//! `KATALOGR_QUERY_PROJECT` name an index that is already built and is only read.
//! `KATALOGR_QUERY_REPS` (default 5) is the number of runs per command.

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
            let (ms_limit, mib_limit) = limits(&spec, group, command);
            let (mut walls, mut peak) = (Vec::new(), 0.0f64);
            let load = load_average();
            for _ in 0..reps {
                let mut cmd = k.cmd(args);
                cmd.env("KATALOGR_THREADS", "4");
                let run = run_with_timeout(&mut cmd, Duration::from_secs(600)).expect("run");
                assert_eq!(
                    run.status,
                    Some(0),
                    "{command}: {}",
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
}
