//! FEAT-059 R-8: exporting contract nodes costs little.
mod support;

use camino::Utf8PathBuf;
use std::time::{Duration, Instant};
use support::{drained_graph, full_index, init_project, open_engine};

use katalogr_engine::doctor::{DoctorOptions, run};
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_model::RelPath;
use katalogr_store::{Reader, ShardStat};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn median_export(with_contracts: bool) -> (Duration, usize) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut text = String::new();
    for i in 0..1000 {
        text.push_str(&format!("sym fn f{i}\n"));
    }
    for i in 0..1000 {
        if with_contracts {
            text.push_str(&format!("bind f{i} provides channel:c{i}\n"));
        }
    }
    std::fs::write(work.join("big.tx"), text).unwrap();
    let kx = root.join("kx");
    init_project(&kx);
    let e = open_engine(&kx, &work);
    full_index(&e);
    let mut times: Vec<Duration> = (0..9)
        .map(|_| {
            let t = Instant::now();
            let g = drained_graph(&e);
            std::hint::black_box(&g);
            t.elapsed()
        })
        .collect();
    times.sort();
    let contracts = drained_graph(&e).matches(r#""node":"contract""#).count();
    (times[4], contracts)
}

mod bench {
    use super::*;

    /// Both graphs have 1,000 symbols; one adds 1,000 bindings, each to its own contract.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn export_1000_contracts() {
        let (base, n1) = median_export(false); // same symbols, no bindings
        let (many, n2) = median_export(true);
        println!("no bindings: {base:?} ({n1} contract), 1000 contracts: {many:?} ({n2})");
        assert_eq!((n1, n2), (0, 1000));
        assert!(
            many <= base + Duration::from_millis(20),
            "{many:?} vs {base:?}"
        );
    }

    /// FEAT-064 R-15: `doctor` on 100,000 shards over 10 repos (5 KB each, stat-matching).
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn doctor_100k() {
        const SHARDS: usize = 100_000;
        const REPOS: usize = 10;
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let kx = root.join("kx");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        support::init_project(&kx);
        let dir = {
            use katalogr_store::{Kindex, ProjectName};
            Kindex::open(&kx)
                .unwrap()
                .project(&ProjectName::new("p").unwrap())
                .unwrap()
        };
        // A project with ten repositories (the helper makes one; rewrite its config).
        let repos: Vec<String> = (0..REPOS).map(|i| format!("r{i}")).collect();
        let config = format!(
            "repos:\n{}",
            repos
                .iter()
                .map(|r| format!("- name: {r}\n  include: ['**']\n  exclude: []\n"))
                .collect::<String>()
        );
        std::fs::write(dir.root().join("config.yaml"), config).unwrap();
        let dir = {
            use katalogr_store::{Kindex, ProjectName};
            Kindex::open(&kx)
                .unwrap()
                .project(&ProjectName::new("p").unwrap())
                .unwrap()
        };

        // Shard files and their hash rows. The engine opens the (empty) project first, so the
        // open does not try to decode the synthetic shards.
        let t = Instant::now();
        let e = open_engine(&kx, &work);
        let mut rows = Vec::with_capacity(SHARDS);
        let mut body = vec![b' '; 5000];
        for i in 0..SHARDS {
            let (repo, path) = (&repos[i % REPOS], format!("d{}/f{i}.tx", i / 1000));
            let key = RelPath::new(&path).unwrap();
            let file = dir.shard_path(repo, &key);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            let text = format!("{{\"n\":{i}}}");
            body[..text.len()].copy_from_slice(text.as_bytes());
            std::fs::write(&file, &body).unwrap();
            let stat = ShardStat::of(&std::fs::metadata(&file).unwrap()).unwrap();
            let hash: [u8; 32] = Sha256::digest(&body).into();
            rows.push((repo.clone(), key, hash, stat));
        }
        {
            // The rows go in with one transaction.
            let mut tx = e.cache().write().unwrap();
            for (repo, key, hash, stat) in &rows {
                tx.put_shard_hash_stat(repo, key, hash, *stat).unwrap();
            }
            tx.commit().unwrap();
            assert_eq!(
                e.cache().read().unwrap().shard_hashes().unwrap().len(),
                SHARDS
            );
        }
        drop(e);
        // The kindex is committed, so the git check has nothing to report.
        for args in [&["add", "-A"][..], &["commit", "-q", "-m", "shards"][..]] {
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
                .current_dir(&kx)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        println!("built {SHARDS} shards in {:?}", t.elapsed());

        let mut registry = ExtensionRegistry::new();
        registry.register(Box::new(support::TestExtractor));
        let roots: BTreeMap<String, Utf8PathBuf> =
            repos.iter().map(|r| (r.clone(), work.clone())).collect();
        let time = |skip_git: bool| {
            let opts = DoctorOptions {
                skip_git,
                ..Default::default()
            };
            let t = Instant::now();
            let r = run(&dir, &registry, &roots, None, &opts).unwrap();
            (t.elapsed(), r)
        };
        let _warm = time(true);
        let (with_git, r) = time(false);
        assert!(r.findings.is_empty(), "{:?}", r.findings);
        let (without, r) = time(true);
        assert!(r.findings.is_empty(), "{:?}", r.findings);
        println!("doctor on {SHARDS} shards: {with_git:?} with git, {without:?} with --skip-git");
        assert!(with_git < Duration::from_millis(5000), "{with_git:?}");
        assert!(without < Duration::from_millis(1500), "{without:?}");
    }

    /// FEAT-077 R-11: 5,000 references that each resolve to the same 100 targets stitch in
    /// 5 s, stored as 5,000 fan rows and no per-target `calls` link row.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn fanout_stitch_budget() {
        use katalogr_engine::Stitcher;
        use katalogr_model::{ExtractInput, Extractor};
        use katalogr_store::cache::TableId;
        use katalogr_store::{Cache, Kindex, ProjectConfig, ProjectName, RepoConfig};

        const FILES: usize = 50;
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("p").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let stitcher = Stitcher::new();
        let cache =
            Cache::open_or_rebuild(&dir, &stitcher, Instant::now() + Duration::from_secs(60))
                .unwrap();
        let fragments: Vec<_> = (0..FILES)
            .map(|f| {
                let mut text = String::new();
                for e in 0..2 {
                    text.push_str(&format!("sym fn e{e}\nexport m:area e{e}\n"));
                }
                for c in 0..100 {
                    text.push_str(&format!("sym fn c{c}\nref c{c} calls m:area\n"));
                }
                let path = RelPath::new(&format!("f{f}.tx")).unwrap();
                support::TestExtractor
                    .extract(&ExtractInput {
                        repo: "r",
                        path: &path,
                        revision: "ce013625030ba8dba906f756967f9e9ca394464a",
                        content: text.as_bytes(),
                        context: &[],
                    })
                    .unwrap()
            })
            .collect();
        let t = Instant::now();
        for f in fragments {
            let mut tx = cache.write().unwrap();
            stitcher.replace_fragment(&mut tx, f).unwrap();
            tx.commit().unwrap();
        }
        let took = t.elapsed();
        let rx = cache.read().unwrap();
        let fans = rx.fans_all().unwrap().len();
        let stored = rx.raw_scan(TableId::LinksOut, &[]).unwrap().len();
        let expanded = rx.links_all().unwrap().len();
        println!(
            "stitched {FILES} files in {took:?}: {fans} fan rows, {stored} stored link rows, {expanded} expanded"
        );
        assert_eq!((fans, stored, expanded), (5000, 0, 500_000));
        assert!(took < Duration::from_millis(5000), "{took:?}");
    }

    /// A fresh cache and stitcher over a one-repository project.
    fn stitch_env(tmp: &tempfile::TempDir) -> (katalogr_store::Cache, katalogr_engine::Stitcher) {
        use katalogr_engine::Stitcher;
        use katalogr_store::{Cache, Kindex, ProjectConfig, ProjectName, RepoConfig};
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("p").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let stitcher = Stitcher::new();
        let cache =
            Cache::open_or_rebuild(&dir, &stitcher, Instant::now() + Duration::from_secs(60))
                .unwrap();
        (cache, stitcher)
    }

    fn load_average() -> String {
        std::fs::read_to_string("/proc/loadavg")
            .map(|s| s.split_whitespace().next().unwrap_or("?").to_string())
            .unwrap_or_else(|_| "?".into())
    }

    /// FEAT-093 R-12: files that each export and reference one stable key stitch in time and
    /// re-resolutions that grow linearly with the file count, not quadratically.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn stable_key_stitch_scales_linearly() {
        use katalogr_model::{ExtractInput, Extractor};
        let run = |files: usize| {
            let tmp = tempfile::tempdir().unwrap();
            let (cache, stitcher) = stitch_env(&tmp);
            let fragments: Vec<_> = (0..files)
                .map(|f| {
                    let text = "sym fn e\nexport m:area e\nsym fn c\nref c calls m:area\n";
                    let path = RelPath::new(&format!("f{f}.tx")).unwrap();
                    support::TestExtractor
                        .extract(&ExtractInput {
                            repo: "r",
                            path: &path,
                            revision: "ce013625030ba8dba906f756967f9e9ca394464a",
                            content: text.as_bytes(),
                            context: &[],
                        })
                        .unwrap()
                })
                .collect();
            let t = Instant::now();
            let mut resolved = 0usize;
            for f in fragments {
                let mut tx = cache.write().unwrap();
                resolved += stitcher
                    .replace_fragment(&mut tx, f)
                    .unwrap()
                    .refs_resolved
                    .len();
                tx.commit().unwrap();
            }
            (t.elapsed(), resolved)
        };
        let (t1, r1) = run(1000);
        let (t2, r2) = run(2000);
        let time_ratio = t2.as_secs_f64() / t1.as_secs_f64();
        let resolve_ratio = r2 as f64 / r1 as f64;
        println!(
            "load {}: 1000 files {t1:?} ({r1} re-resolves), 2000 files {t2:?} ({r2}); time x{time_ratio:.2}, re-resolves x{resolve_ratio:.2}",
            load_average()
        );
        assert!(resolve_ratio <= 2.2, "re-resolves x{resolve_ratio:.2}");
        assert!(time_ratio <= 3.0, "time x{time_ratio:.2}");
    }

    /// FEAT-093 R-13: ranking 8 million fan-row edges adds at most 800 MiB of resident memory.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    #[cfg(target_os = "linux")]
    fn rank_fan_expansion_memory() {
        use katalogr_model::{ExtractInput, Extractor};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

        fn rss_kib() -> u64 {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|r| r.split_whitespace().next()?.parse().ok())
                .unwrap_or(0)
        }

        const SOURCES: usize = 4000;
        const TARGETS: usize = 2000;
        let tmp = tempfile::tempdir().unwrap();
        let (cache, stitcher) = stitch_env(&tmp);
        let mut text = String::new();
        for e in 0..TARGETS {
            text.push_str(&format!("sym fn e{e}\nexport m:area e{e}\n"));
        }
        for c in 0..SOURCES {
            text.push_str(&format!("sym fn c{c}\nref c{c} calls m:area\n"));
        }
        let path = RelPath::new("big.tx").unwrap();
        let fragment = support::TestExtractor
            .extract(&ExtractInput {
                repo: "r",
                path: &path,
                revision: "ce013625030ba8dba906f756967f9e9ca394464a",
                content: text.as_bytes(),
                context: &[],
            })
            .unwrap();
        let mut tx = cache.write().unwrap();
        stitcher.replace_fragment(&mut tx, fragment).unwrap();
        tx.commit().unwrap();

        let rx = cache.read().unwrap();
        let symbols = rx.symbols().unwrap();
        assert_eq!(rx.fans_all().unwrap().len(), SOURCES);
        let base = rss_kib();
        let peak = Arc::new(AtomicU64::new(base));
        let stop = Arc::new(AtomicBool::new(false));
        let sampler = {
            let (peak, stop) = (peak.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    peak.fetch_max(rss_kib(), Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        };
        let t = Instant::now();
        let scores = katalogr_store::pagerank(&rx, &symbols).unwrap();
        let took = t.elapsed();
        stop.store(true, Ordering::Relaxed);
        sampler.join().unwrap();
        std::hint::black_box(&scores);
        let grown_mib = (peak.load(Ordering::Relaxed).saturating_sub(base)) / 1024;
        println!(
            "load {}: rank of {} edges in {took:?}, peak +{grown_mib} MiB",
            load_average(),
            SOURCES * TARGETS
        );
        assert!(grown_mib <= 800, "+{grown_mib} MiB");
    }

    /// FEAT-093 R-14: `put_exports` costs time linear in the list, so a 4x longer list costs at
    /// most 6x.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn put_exports_scales_linearly() {
        use katalogr_model::{ExportKey, Id};
        let tmp = tempfile::tempdir().unwrap();
        let (cache, _stitcher) = stitch_env(&tmp);
        let key = ExportKey {
            namespace: "m".into(),
            key: "area".into(),
        };
        let ids = |n: usize| -> Vec<Id> {
            (0..n as u128)
                .map(|i| {
                    let mut b = [0u8; 16];
                    b.copy_from_slice(&(i * 2 + 1).to_be_bytes());
                    Id(b)
                })
                .collect()
        };
        let run = |n: usize| {
            let (short, long) = (ids(n), ids(n + 1));
            let mut tx = cache.write().unwrap();
            tx.put_exports(&key, &short).unwrap();
            let t = Instant::now();
            for _ in 0..10 {
                tx.put_exports(&key, &long).unwrap();
                tx.put_exports(&key, &short).unwrap();
            }
            let took = t.elapsed();
            tx.commit().unwrap();
            took
        };
        let small = run(5000);
        let large = run(20000);
        let ratio = large.as_secs_f64() / small.as_secs_f64();
        println!(
            "load {}: 5000 targets {small:?}, 20000 targets {large:?}, x{ratio:.2}",
            load_average()
        );
        assert!(ratio <= 6.0, "x{ratio:.2}");
    }

    /// FEAT-076 R-13: the `symbol_core` table adds at most 10% to the stitch and 12% to the
    /// cache. 100,000 symbols are stitched; the stitch is timed whole, and the price of the core
    /// rows is the difference between writing the same symbol rows with and without them.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn symbol_core_stitch_overhead() {
        use katalogr_model::{ExtractInput, Extractor};
        use katalogr_store::cache::TableId;
        const FILES: usize = 100;
        const PER_FILE: usize = 1000;
        let tmp = tempfile::tempdir().unwrap();
        let (cache, stitcher) = stitch_env(&tmp);
        let fragments: Vec<_> = (0..FILES)
            .map(|f| {
                let text: String = (0..PER_FILE)
                    .map(|i| format!("sym fn function_{f}_{i}\n"))
                    .collect();
                let path = RelPath::new(&format!("d{}/f{f}.tx", f % 10)).unwrap();
                support::TestExtractor
                    .extract(&ExtractInput {
                        repo: "r",
                        path: &path,
                        revision: "ce013625030ba8dba906f756967f9e9ca394464a",
                        content: text.as_bytes(),
                        context: &[],
                    })
                    .unwrap()
            })
            .collect();
        let started = Instant::now();
        let mut tx = cache.write().unwrap();
        for f in fragments {
            stitcher.replace_fragment(&mut tx, f).unwrap();
        }
        tx.commit().unwrap();
        let stitch = started.elapsed();

        let rx = cache.read().unwrap();
        let symbols = rx.symbols().unwrap();
        let core: u64 = rx
            .raw_scan(TableId::SymbolCore, &[])
            .unwrap()
            .iter()
            .map(|(k, v)| (k.len() + v.len()) as u64)
            .sum();
        drop(rx);
        let file = std::fs::metadata(tmp.path().join("kx/projects/p/.cache/index.redb"))
            .expect("the cache file")
            .len();
        // The price of the core rows: the same writes into fresh caches, with and without them.
        let write_all = |with_core: bool| {
            let t = tempfile::tempdir().unwrap();
            let (cache, _) = stitch_env(&t);
            let started = Instant::now();
            let mut tx = cache.write().unwrap();
            for (id, row) in &symbols {
                if with_core {
                    tx.put_symbol(*id, row).unwrap();
                } else {
                    tx.put_symbol_row_only(*id, row).unwrap();
                }
            }
            tx.commit().unwrap();
            started.elapsed()
        };
        let mut extra = Vec::new();
        for _ in 0..3 {
            let (with, without) = (write_all(true), write_all(false));
            extra.push(with.as_secs_f64() - without.as_secs_f64());
        }
        extra.sort_by(|a, b| a.total_cmp(b));
        let extra = extra[1].max(0.0);
        let base = (stitch.as_secs_f64() - extra).max(f64::MIN_POSITIVE);
        let time_pct = extra / base * 100.0;
        let size_pct = core as f64 / (file.saturating_sub(core).max(1)) as f64 * 100.0;
        println!(
            "load {}: {} symbols, stitch {stitch:?} (core rows cost {:.0} ms = +{time_pct:.1}%), \
             cache file {file} bytes, core rows {core} bytes = +{size_pct:.1}%",
            load_average(),
            symbols.len(),
            extra * 1000.0,
        );
        assert!(time_pct <= 10.0, "stitch +{time_pct:.1}%");
        assert!(size_pct <= 12.0, "cache +{size_pct:.1}%");
    }
}
