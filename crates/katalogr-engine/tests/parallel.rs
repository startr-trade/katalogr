//! FEAT-072: parallel indexing. Results are byte-identical to a 1-thread run; the round
//! honours the byte cap and the deadline; failures leave no visible state.
mod support;

#[cfg(unix)]
use camino::Utf8Path;
use camino::Utf8PathBuf;
use katalogr_engine::Scope;
use katalogr_engine::consistency::DEFAULT_BUDGET;
use katalogr_engine::parallel::Parallelism;
use katalogr_engine::queue::Processor;
#[cfg(unix)]
use katalogr_engine::queue::find_orphan_temps;
use katalogr_engine::registry::ExtensionRegistry;
#[cfg(unix)]
use katalogr_engine::{Engine, canonical_graph};
use katalogr_store::Reader;
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};
use support::{
    CacheDump, TestExtractor, TestProbe, cache_dump, dictionary_dump, drained_graph, full_index,
    init_project, open_engine_par, open_engine_threads, shard_bytes, test_slow,
};

struct Project {
    _tmp: tempfile::TempDir,
    kx: Utf8PathBuf,
    work: Utf8PathBuf,
}

fn project() -> Project {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let kx = root.join("kx");
    init_project(&kx);
    Project {
        _tmp: tmp,
        kx,
        work,
    }
}

/// A file with symbols, exports (one key shared by every file), references, bindings, a bound
/// reference and a guest text.
fn rich(i: usize, n: usize) -> String {
    // Every file names `shared:common`; a tenth of them export it, so the key changes often.
    let shared = if i.is_multiple_of(10) {
        "export shared:common G0\n".replace("G0", &format!("G{i}"))
    } else {
        String::new()
    };
    format!(
        "sym fn F{i}\nsym fn G{i}\nexport ns:k{i} F{i}\n{shared}\
         ref F{i} calls ns:k{}\nref G{i} calls shared:common\nref G{i} calls ns:missing{}\n\
         link F{i} calls G{i}\nbind F{i} provides res:r{}\n\
         boundref G{i} consumes res:r{} ns:k{}\nlit F{i} use w{}\n",
        (i * 7 + 1) % n,
        i % 5,
        i % 10,
        (i + 3) % 10,
        (i * 3) % n,
        i % 5,
    )
}

type Snapshot = (BTreeMap<String, Vec<u8>>, String, CacheDump);

/// Index `files` (path, text) fully at `threads` threads and capture shards, graph and cache.
fn index_at(files: &[(String, String)], threads: usize) -> Snapshot {
    let p = project();
    for (path, text) in files {
        let f = p.work.join(path);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, text).unwrap();
    }
    let e = open_engine_threads(&p.kx, &p.work, threads);
    let graph = full_index(&e);
    let mut dump = cache_dump(&e);
    dump.extend(dictionary_dump(&e));
    (shard_bytes(&p.kx), graph, dump)
}

fn rich_files(n: usize) -> Vec<(String, String)> {
    (0..n)
        .map(|i| (format!("d{}/f{i}.tx", i % 7), rich(i, n)))
        .collect()
}

mod parallel {
    use super::*;

    /// R-1. The work of a round runs on the engine's pool, not on the caller.
    #[test]
    fn plan_runs_on_pool() {
        let p = project();
        for i in 0..64 {
            std::fs::write(p.work.join(format!("p{i}.probe")), format!("sym fn P{i}\n")).unwrap();
        }
        let probe = TestProbe {
            id: "probe",
            suffix: ".probe",
            delay: Duration::from_millis(5),
            seen: Default::default(),
        };
        let seen = probe.seen.clone();
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(probe));
        let par = Parallelism {
            threads: NonZeroUsize::new(4).unwrap(),
            ..Parallelism::sequential()
        };
        let e = open_engine_par(&p.kx, &p.work, reg, par);
        full_index(&e);
        let names = seen.lock().unwrap().clone();
        assert!(
            names.len() > 1 && names.iter().all(|n| n.starts_with("katalogr-index-")),
            "{names:?}"
        );
        assert_eq!(e.dir().list_shards().unwrap().len(), 64);
        let stats = e.index_stats();
        assert_eq!(stats.files, 64);
        assert!(stats.rounds >= 1);
    }

    /// R-2. Shards, canonical graph and cache contents do not depend on the thread count.
    #[test]
    fn one_vs_many_threads_identical() {
        let files = rich_files(200);
        let one = index_at(&files, 1);
        assert!(one.0.len() >= 200, "shards written");
        for threads in [2, 8] {
            let many = index_at(&files, threads);
            assert!(one.0 == many.0, "shard bytes differ at {threads} threads");
            assert_eq!(one.1, many.1, "canonical graph at {threads} threads");
            assert!(one.2 == many.2, "cache dump differs at {threads} threads");
        }
    }

    /// FEAT-093 R-8. Many files that share keys, with fan rows and bound references: shards,
    /// graph and the whole cache, rank included, do not depend on the thread count.
    #[test]
    fn fan_heavy_one_vs_eight_threads_identical() {
        let files: Vec<(String, String)> = (0..300)
            .map(|i| {
                // Every file names the hubs; a tenth of the files export each, so the hubs
                // hold fan rows and change often, and bound references follow them.
                let hub = if i % 10 == 0 {
                    format!("export shared:hub F{i}\n")
                } else if i % 10 == 5 {
                    format!("export shared:hub2 F{i}\nexport shared:hub G{i}\n")
                } else {
                    String::new()
                };
                let text = format!(
                    "sym fn F{i}\nsym fn G{i}\nexport shared:k{} G{i}\n{hub}\
                     refs G{i} calls shared:k{} shared:hub\nref F{i} calls shared:k{}\n\
                     ref F{i} uses shared:hub2\n",
                    i % 12,
                    (i + 5) % 12,
                    i % 12
                );
                let text = if i % 3 == 0 {
                    format!("{text}boundref G{i} consumes res:r{} shared:hub\n", i % 9)
                } else {
                    text
                };
                (format!("d{}/h{i}.tx", i % 5), text)
            })
            .collect();
        let one = index_at(&files, 1);
        let many = index_at(&files, 8);
        assert!(
            one.2
                .iter()
                .any(|(n, rows)| n == "fan_out" && !rows.is_empty()),
            "fan rows exist"
        );
        assert!(
            one.2
                .iter()
                .any(|(n, rows)| n == "rank" && !rows.is_empty()),
            "rank is part of the dump"
        );
        assert!(one.0 == many.0, "shard bytes differ");
        assert_eq!(one.1, many.1, "canonical graph");
        assert!(one.2 == many.2, "cache dump differs");
    }

    fn tree() -> impl Strategy<Value = Vec<(String, String)>> {
        let file = (0usize..30, 0usize..6, 0usize..6, 0usize..4).prop_map(|(i, k, r, b)| {
            let text = format!(
                "sym fn A{i}\nsym fn B{i}\nexport k:e{k} A{i}\nref B{i} calls k:e{r}\n\
                 ref A{i} calls k:e{k}\nbind B{i} consumes res:c{b}\nlink A{i} calls B{i}\n"
            );
            (format!("g{}/t{i}.tx", i % 3), text)
        });
        prop::collection::vec(file, 1..30).prop_map(|mut v| {
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v.dedup_by(|a, b| a.0 == b.0);
            v
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]

        /// R-2. Random trees give identical results at 1 and 4 threads.
        #[test]
        fn threads_property_identical(files in tree()) {
            let one = index_at(&files, 1);
            let four = index_at(&files, 4);
            prop_assert!(one.0 == four.0, "shard bytes");
            prop_assert_eq!(one.1, four.1);
            prop_assert!(one.2 == four.2, "cache dump");
        }
    }

    /// R-4. A round stops adding waves once its encoded bytes reach `batch_bytes`; the rest
    /// stays pending and the final graph equals the 1-thread graph.
    #[test]
    fn batch_bytes_caps_round() {
        let p = project();
        for i in 0..40 {
            let mut text = String::new();
            for s in 0..400 {
                text.push_str(&format!("sym fn S{i}x{s}\nexport ns:s{i}x{s} S{i}x{s}\n"));
            }
            std::fs::write(p.work.join(format!("big{i:02}.tx")), text).unwrap();
        }
        let par = Parallelism {
            threads: NonZeroUsize::new(4).unwrap(),
            batch_bytes: 1 << 20,
        };
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(TestExtractor));
        let e = open_engine_par(&p.kx, &p.work, reg, par);
        e.inbox()
            .append(&katalogr_engine::NewEvent::rescan("r"))
            .unwrap();
        let mut pr = Processor::try_acquire(&e).unwrap().unwrap();
        pr.ingest().unwrap();
        pr.process(
            &Scope::Project,
            DEFAULT_BUDGET,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        drop(pr);
        let published = e.dir().list_shards().unwrap().len();
        let pending = e
            .cache()
            .read()
            .unwrap()
            .pending_entries(None)
            .unwrap()
            .len();
        assert!(published > 0 && published < 40, "published {published}");
        assert_eq!(published + pending, 40);
        let graph = drained_graph(&e);

        let q = project();
        for i in 0..40 {
            std::fs::copy(
                p.work.join(format!("big{i:02}.tx")),
                q.work.join(format!("big{i:02}.tx")),
            )
            .unwrap();
        }
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(TestExtractor));
        let e1 = open_engine_par(&q.kx, &q.work, reg, Parallelism::sequential());
        assert!(
            graph == full_index(&e1),
            "graph differs from the 1-thread graph"
        );
    }

    /// R-5. At the deadline a round publishes a non-empty prefix of its ordered paths.
    #[test]
    fn deadline_publishes_prefix() {
        let p = project();
        for i in 0..100 {
            std::fs::write(
                p.work.join(format!("s{i:03}.slow")),
                format!("sym fn S{i}\n"),
            )
            .unwrap();
        }
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(test_slow()));
        let par = Parallelism {
            threads: NonZeroUsize::new(4).unwrap(),
            ..Parallelism::sequential()
        };
        let e = open_engine_par(&p.kx, &p.work, reg, par);
        e.inbox()
            .append(&katalogr_engine::NewEvent::rescan("r"))
            .unwrap();
        let mut pr = Processor::try_acquire(&e).unwrap().unwrap();
        pr.ingest().unwrap();
        pr.process(
            &Scope::Project,
            DEFAULT_BUDGET,
            Instant::now() + Duration::from_millis(50),
        )
        .unwrap();
        drop(pr);

        let snap = e.cache().read().unwrap();
        let pending = snap.pending_entries(None).unwrap();
        let published = e.dir().list_shards().unwrap();
        assert!(!published.is_empty(), "a non-empty prefix is published");
        assert!(!pending.is_empty(), "the deadline left paths pending");
        assert_eq!(published.len() + pending.len(), 100);
        let last_published = published.iter().map(|(_, p)| p.clone()).max().unwrap();
        assert!(
            pending.iter().all(|(k, _)| k.path > last_published),
            "published paths are a prefix of the ordered batch"
        );
        let lowest = pending.iter().map(|(_, s)| *s).min().unwrap();
        assert!(snap.watermark("worktree").unwrap() < lowest);
    }

    /// Make `path` unreadable; false when the process can still read it (running as root).
    #[cfg(unix)]
    fn unreadable(path: &Utf8Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::read(path).is_err()
    }

    #[cfg(unix)]
    fn index_error(threads: usize, bad: &[&str]) -> (Engine, Project, String) {
        let p = project();
        for i in 0..24 {
            std::fs::write(p.work.join(format!("f{i:02}.tx")), format!("sym fn F{i}\n")).unwrap();
        }
        for b in bad {
            assert!(unreadable(&p.work.join(b)), "file is still readable");
        }
        let e = open_engine_threads(&p.kx, &p.work, threads);
        e.inbox()
            .append(&katalogr_engine::NewEvent::rescan("r"))
            .unwrap();
        let mut pr = Processor::try_acquire(&e).unwrap().unwrap();
        pr.ingest().unwrap();
        let err = pr
            .process(
                &Scope::Project,
                DEFAULT_BUDGET,
                Instant::now() + Duration::from_secs(30),
            )
            .expect_err("an unreadable file fails the round")
            .to_string();
        drop(pr);
        (e, p, err)
    }

    /// R-6. A planning error mid-round leaves the graph and watermark unchanged and no temp
    /// files after recovery.
    #[cfg(unix)]
    #[test]
    fn plan_error_leaves_no_visible_state() {
        for threads in [1, 4] {
            let (e, p, _) = index_error(threads, &["f13.tx"]);
            {
                let snap = e.cache().read().unwrap();
                assert_eq!(snap.watermark("worktree").unwrap(), 0);
                assert!(snap.dump_graph().unwrap().iter().all(|(n, rows)| {
                    // Only the pending queue changed; no artifact, symbol or link exists.
                    !["artifacts", "symbols", "links_out"].contains(n) || rows.is_empty()
                }));
                assert!(snap.shard_hashes().unwrap().is_empty());
            }
            let mut pr = Processor::try_acquire(&e).unwrap().unwrap();
            pr.recover().unwrap();
            drop(pr);
            assert!(e.dir().list_shards().unwrap().is_empty());
            let shards = p.kx.join("projects/p/shards");
            assert!(find_orphan_temps(&shards).unwrap().is_empty(), "{threads}");
            assert_eq!(
                canonical_graph(&e.cache().read().unwrap()),
                canonical_empty()
            );
        }
    }

    #[cfg(unix)]
    fn canonical_empty() -> String {
        let p = project();
        let e = open_engine_threads(&p.kx, &p.work, 1);
        canonical_graph(&e.cache().read().unwrap())
    }

    /// R-7. The error reported is the first in batch order, at any thread count.
    #[cfg(unix)]
    #[test]
    fn first_error_in_batch_order() {
        for threads in [1, 8] {
            let (_e, _p, err) = index_error(threads, &["f05.tx", "f19.tx"]);
            assert!(err.contains("f05.tx"), "{threads}: {err}");
            assert!(!err.contains("f19.tx"), "{threads}: {err}");
        }
    }
}
