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

mod bench {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_bench::katalogr::Katalogr;
    use katalogr_bench::proc::run_with_timeout;
    use katalogr_bench::sizes::{cache_bytes, shard_stats};
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
}
