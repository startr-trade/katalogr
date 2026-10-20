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
}
