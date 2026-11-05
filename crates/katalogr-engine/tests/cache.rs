mod support;

use camino::Utf8PathBuf;
use katalogr_engine::Stitcher;
use katalogr_model::{ExtractInput, Extractor, Fragment, RelPath};
use katalogr_store::{Cache, Kindex, ProjectConfig, ProjectDir, ProjectName, Reader, RepoConfig};
use std::time::{Duration, Instant};
use support::TestExtractor;

fn project(tmp: &tempfile::TempDir) -> ProjectDir {
    let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
    Kindex::init(&root)
        .unwrap()
        .create_project(
            &ProjectName::new("p").unwrap(),
            ProjectConfig {
                repos: vec![RepoConfig::new("r")],
                ..Default::default()
            },
        )
        .unwrap()
}

fn extract(path: &str, text: &str) -> Fragment {
    let path = RelPath::new(path).unwrap();
    TestExtractor
        .extract(&ExtractInput {
            repo: "r",
            path: &path,
            revision: "ce013625030ba8dba906f756967f9e9ca394464a",
            content: text.as_bytes(),
            context: &[],
        })
        .unwrap()
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

mod kindex {
    use super::*;

    #[test]
    fn cache_rebuilds_from_shards() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        for (p, t) in [
            ("a.tx", "sym fn A\nexport ns:k A\n"),
            (
                "b.tx",
                "sym fn B\nsym fn B2\nlink B calls B2\nref B calls ns:k\n",
            ),
            ("c.tx", "sym fn C\nref C calls ns:nope\n"),
        ] {
            dir.write_shard(&extract(p, t)).unwrap();
        }

        let stitcher = Stitcher::new();
        let cache = Cache::open_or_rebuild(&dir, &stitcher, soon()).unwrap();
        assert_eq!(cache.report().replaced, 3);
        let before = cache.read().unwrap().dump_graph().unwrap();
        let rows = |name: &str| {
            before
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, r)| r.len())
                .unwrap()
        };
        assert_eq!(rows("artifacts"), 3);
        assert_eq!(rows("symbols"), 4);
        assert_eq!(rows("links_out"), 2);
        assert_eq!(rows("refs_unresolved"), 1);
        drop(cache);

        // Warm open: nothing changed, nothing replayed.
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().replaced, 0);
        assert_eq!(cache.read().unwrap().dump_graph().unwrap(), before);
        drop(cache);

        // Delete the cache; rebuilding from the shards gives the same graph.
        std::fs::remove_dir_all(dir.cache_dir()).unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().replaced, 3);
        assert_eq!(cache.read().unwrap().dump_graph().unwrap(), before);
        drop(cache);

        // A shard removed on disk is removed from the cache; same result as a fresh build.
        std::fs::remove_file(dir.shard_path("r", &RelPath::new("a.tx").unwrap())).unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().removed, 1);
        let incremental = cache.read().unwrap().dump_graph().unwrap();
        drop(cache);
        std::fs::remove_dir_all(dir.cache_dir()).unwrap();
        let fresh = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(fresh.read().unwrap().dump_graph().unwrap(), incremental);
        assert_ne!(incremental, before);
    }
}

mod cache {
    use super::*;
    use katalogr_store::StoreError;

    #[test]
    fn warm_open_skips_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        for (p, t) in [("a.tx", "sym fn A\n"), ("b.tx", "sym fn B\n")] {
            dir.write_shard(&extract(p, t)).unwrap();
        }
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().hashed, 2);
        drop(cache);

        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().hashed, 0);
        assert_eq!(cache.report().replaced, 0);
        drop(cache);

        dir.write_shard(&extract("a.tx", "sym fn A\nsym fn A2\n"))
            .unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().hashed, 1);
        assert_eq!(cache.report().replaced, 1);
    }

    fn commits() -> u64 {
        katalogr_store::cache::commits_on_this_thread()
    }

    #[test]
    fn open_unchanged_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        for (p, t) in [("a.tx", "sym fn A\n"), ("b.tx", "sym fn B\n")] {
            dir.write_shard(&extract(p, t)).unwrap();
        }
        let first = commits();
        drop(Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap());
        assert!(commits() > first, "the cold open commits");
        let settled = commits();
        for i in 0..3 {
            let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
            assert_eq!(cache.report().hashed, 0);
            assert_eq!(cache.report().replaced, 0);
            assert_eq!(cache.report().removed, 0);
            drop(cache);
            assert_eq!(commits(), settled, "warm open {i} commits nothing");
        }
    }

    #[test]
    fn open_changed_shard_still_syncs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        for (p, t) in [("a.tx", "sym fn A\n"), ("b.tx", "sym fn B\n")] {
            dir.write_shard(&extract(p, t)).unwrap();
        }
        drop(Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap());

        // A changed shard.
        dir.write_shard(&extract("a.tx", "sym fn A\nsym fn A2\n"))
            .unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().replaced, 1);
        assert_eq!(cache.read().unwrap().symbols().unwrap().len(), 3);
        drop(cache);

        // A new shard.
        dir.write_shard(&extract("c.tx", "sym fn C\n")).unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().replaced, 1);
        assert_eq!(cache.read().unwrap().symbols().unwrap().len(), 4);
        drop(cache);

        // A removed shard.
        std::fs::remove_file(dir.shard_path("r", &RelPath::new("b.tx").unwrap())).unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().removed, 1);
        assert_eq!(cache.read().unwrap().symbols().unwrap().len(), 3);
        drop(cache);

        // Settled again: the next open writes nothing.
        let before = commits();
        drop(Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap());
        assert_eq!(commits(), before);
    }

    #[test]
    fn schema_8_cache_rebuilds() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        for (p, t) in [
            ("a.tx", "sym fn A\nexport ns:k A\n"),
            ("b.tx", "sym fn B\nref B calls ns:k\n"),
            ("c.tx", "sym fn C\nref C calls ns:k\n"),
        ] {
            dir.write_shard(&extract(p, t)).unwrap();
        }
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        let before = cache.read().unwrap().dump_graph().unwrap();
        {
            let mut tx = cache.write().unwrap();
            tx.set_meta("cache_schema", &8u32.to_le_bytes()).unwrap();
            tx.commit().unwrap();
        }
        drop(cache);

        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().replaced, 3, "rebuilt from the shards");
        assert_eq!(cache.read().unwrap().dump_graph().unwrap(), before);
    }

    #[test]
    fn schema_9_cache_rebuilds() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        for (p, t) in [
            ("a.tx", "sym fn A\nexport ns:k A\n"),
            ("a2.tx", "sym fn A2\nexport ns:k A2\n"),
            ("b.tx", "sym fn B\nref B calls ns:k\n"),
        ] {
            dir.write_shard(&extract(p, t)).unwrap();
        }
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        let before = cache.read().unwrap().dump_graph().unwrap();
        {
            let mut tx = cache.write().unwrap();
            tx.set_meta("cache_schema", &9u32.to_le_bytes()).unwrap();
            tx.commit().unwrap();
        }
        drop(cache);

        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().replaced, 3, "rebuilt from the shards");
        let after = cache.read().unwrap().dump_graph().unwrap();
        assert_eq!(after, before);
        let fan_rows = after.iter().find(|(n, _)| *n == "fan_out").unwrap().1.len();
        assert_eq!(fan_rows, 1, "the rebuilt cache has the fan row");
    }

    #[test]
    fn schema_12_cache_rebuilds() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        for (p, t) in [
            ("a.tx", "sym fn A\nexport ns:k A\n"),
            ("b.tx", "sym fn B\nref B calls ns:k\nref B calls ns:nope\n"),
        ] {
            dir.write_shard(&extract(p, t)).unwrap();
        }
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        let before = cache.read().unwrap().dump_graph().unwrap();
        {
            let mut tx = cache.write().unwrap();
            tx.set_meta(
                "cache_schema",
                &(katalogr_store::CACHE_SCHEMA - 1).to_le_bytes(),
            )
            .unwrap();
            tx.commit().unwrap();
        }
        drop(cache);
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().replaced, 2, "rebuilt from the shards");
        assert_eq!(cache.read().unwrap().dump_graph().unwrap(), before);
    }

    #[test]
    fn link_order_matches_schema9() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        // Kinds are written in the order zz, a, calls, so their vocabulary ids are not in the
        // order of their length-prefixed bytes (a, zz, calls).
        dir.write_shard(&extract(
            "a.tx",
            "sym fn A\nsym fn B\nsym fn C\nlink A zz B\nlink A a B\nlink A calls C\nlink A calls B\n",
        ))
        .unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        let rx = cache.read().unwrap();
        let kinds = |keys: Vec<katalogr_store::LinkKey>| -> Vec<String> {
            keys.into_iter().map(|k| k.kind.0).collect()
        };
        let all = rx.links_all().unwrap();
        assert_eq!(
            kinds(all.iter().map(|(k, _)| k.clone()).collect()),
            ["a", "zz", "calls", "calls"]
        );
        let a = all[0].0.from;
        assert_eq!(
            kinds(
                rx.links_out(a, None)
                    .unwrap()
                    .into_iter()
                    .map(|(k, _)| k)
                    .collect()
            ),
            ["a", "zz", "calls", "calls"]
        );
        let b = all[0].0.to;
        assert_eq!(kinds(rx.links_in(b).unwrap()), ["a", "zz", "calls"]);
        let mut walked = Vec::new();
        rx.for_each_link(&mut |_, kind, _, _, _| walked.push(kind.to_string()))
            .unwrap();
        assert_eq!(walked, ["a", "zz", "calls", "calls"]);
        // The key order inside one group is by target id.
        let calls = katalogr_model::LinkKind("calls".into());
        let keys = rx.link_keys_out(a, &calls).unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys[0].to < keys[1].to);
    }

    #[test]
    fn unresolved_from_artifact_row() {
        use katalogr_store::cache::TableId;
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        dir.write_shard(&extract("c.tx", "sym fn C\nref C calls ns:nope\n"))
            .unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        let rx = cache.read().unwrap();
        let unresolved = rx.unresolved_refs(None).unwrap();
        assert_eq!(unresolved.len(), 1);
        let (rid, out) = &unresolved[0];
        let art = rx.artifact(rid.artifact).unwrap().unwrap();
        assert_eq!(out, &art.refs[rid.index as usize]);
        assert_eq!(rx.unresolved(*rid).unwrap().as_ref(), Some(out));
        assert_eq!(rx.unresolved_refs(Some(rid.artifact)).unwrap().len(), 1);
        let raw = rx.raw_scan(TableId::RefsUnresolved, &[]).unwrap();
        assert_eq!(raw.len(), 1);
        assert!(raw[0].1.is_empty(), "the stored value is empty");
    }

    fn prov_rows(rx: &impl Reader) -> Vec<Vec<u8>> {
        use katalogr_store::cache::TableId;
        rx.raw_scan(TableId::Prov, &[])
            .unwrap()
            .into_iter()
            .map(|(_, v)| v)
            .collect()
    }

    fn contains(hay: &[u8], needle: &str) -> bool {
        hay.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    #[test]
    fn prov_dictionary_collected_on_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        for (p, t) in [
            (
                "a.tx",
                "sym fn A\nsym fn A2\nlink A calls A2\nexport ns:k A\n",
            ),
            (
                "b.tx",
                "sym fn B\nsym fn B2\nlink B calls B2\nref B calls ns:k\n",
            ),
        ] {
            dir.write_shard(&extract(p, t)).unwrap();
        }
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        let rows = prov_rows(&cache.read().unwrap());
        assert!(rows.iter().any(|v| contains(v, "b.tx")));
        drop(cache);
        std::fs::remove_file(dir.shard_path("r", &RelPath::new("b.tx").unwrap())).unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().removed, 1);
        let rows = prov_rows(&cache.read().unwrap());
        assert!(!rows.is_empty());
        assert!(
            rows.iter().all(|v| !contains(v, "b.tx")),
            "no row names b.tx"
        );
        assert!(rows.iter().any(|v| contains(v, "a.tx")));
    }

    #[test]
    fn prov_dictionary_follows_revision() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        dir.write_shard(&extract("a.tx", "sym fn A\nsym fn A2\nlink A calls A2\n"))
            .unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        drop(cache);
        let path = RelPath::new("a.tx").unwrap();
        let new_rev = "1".repeat(40);
        let frag = TestExtractor
            .extract(&ExtractInput {
                repo: "r",
                path: &path,
                revision: &new_rev,
                content: b"sym fn A\nsym fn A2\nlink A calls A2\nsym fn A3\n",
                context: &[],
            })
            .unwrap();
        dir.write_shard(&frag).unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().replaced, 1);
        let rows: Vec<_> = prov_rows(&cache.read().unwrap())
            .into_iter()
            .filter(|v| contains(v, "a.tx"))
            .collect();
        assert_eq!(rows.len(), 1, "exactly one row for a.tx");
        assert!(contains(&rows[0], &new_rev));
        assert!(!contains(
            &rows[0],
            "ce013625030ba8dba906f756967f9e9ca394464a"
        ));
    }

    #[test]
    fn dump_independent_of_history() {
        use katalogr_store::cache::TableId;
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        let text = |i: usize, v: usize| {
            format!(
                "sym fn F{i}\nsym fn G{i}\nlink F{i} calls G{i}\nexport ns:k{i} F{i}\nref G{i} calls ns:k{}\n{}",
                (i + 1) % 8,
                "sym fn Extra\n".repeat(v % 2)
            )
        };
        // History (b): start from other contents, then edit and delete down to the final files.
        for i in 0..8 {
            dir.write_shard(&extract(&format!("f{i}.tx"), &text(i, 1)))
                .unwrap();
        }
        dir.write_shard(&extract("gone.tx", "sym fn Gone\nexport ns:gone Gone\n"))
            .unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        drop(cache);
        let mut seed = 7u64;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        for round in 0..30 {
            let i = next() % 8;
            if round % 5 == 4 {
                std::fs::remove_file(
                    dir.shard_path("r", &RelPath::new(&format!("f{i}.tx")).unwrap()),
                )
                .ok();
            } else {
                dir.write_shard(&extract(&format!("f{i}.tx"), &text(i, next())))
                    .unwrap();
            }
            let c = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
            drop(c);
        }
        for i in 0..8 {
            dir.write_shard(&extract(&format!("f{i}.tx"), &text(i, 0)))
                .unwrap();
        }
        std::fs::remove_file(dir.shard_path("r", &RelPath::new("gone.tx").unwrap())).unwrap();
        let history = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        let h = history.read().unwrap();
        let hist_dump = h.dump_graph().unwrap();
        let hist_links = h.links_all().unwrap();
        let hist_prov = h.raw_scan(TableId::Prov, &[]).unwrap();
        drop(h);
        drop(history);

        // History (a): one full index of the final files.
        std::fs::remove_dir_all(dir.cache_dir()).unwrap();
        let fresh = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        let f = fresh.read().unwrap();
        assert_eq!(f.dump_graph().unwrap(), hist_dump);
        assert_eq!(f.links_all().unwrap(), hist_links);
        assert_ne!(
            f.raw_scan(TableId::Prov, &[]).unwrap(),
            hist_prov,
            "the dictionaries number their rows by history"
        );
    }

    // ------------------------------------------------ compaction (FEAT-075 T6)

    /// Every table but `meta` as raw rows: a rewrite copies them verbatim.
    fn raw_tables(rx: &impl Reader) -> Vec<(String, Vec<(Vec<u8>, Vec<u8>)>)> {
        use katalogr_store::cache::TableId;
        TableId::ALL
            .iter()
            .filter(|t| **t != TableId::Meta)
            .map(|t| (format!("{t:?}"), rx.raw_scan(*t, &[]).unwrap()))
            .collect()
    }

    /// redb trims the unused tail of a file when it closes it, so the size recorded for the
    /// closed file and the size of the open one differ by a small allocation step.
    fn near(recorded: u64, file: u64) {
        assert!(
            recorded.abs_diff(file) <= 64 * 1024,
            "recorded {recorded}, file {file}"
        );
    }

    fn sample_project(tmp: &tempfile::TempDir) -> ProjectDir {
        let dir = project(tmp);
        for (p, t) in [
            (
                "a.tx",
                "sym fn A\nsym fn A2\nlink A calls A2\nexport ns:k A\n",
            ),
            (
                "b.tx",
                "sym fn B\nsym fn B2\nlink B calls B2\nref B calls ns:k\nref B calls ns:nope\n",
            ),
            ("c.tx", "sym fn C\nexport ns:k C\n"),
        ] {
            dir.write_shard(&extract(p, t)).unwrap();
        }
        dir
    }

    #[test]
    fn rewrite_preserves_dump() {
        use katalogr_store::{CompactMode, CompactOutcome};
        for mode in [CompactMode::Rewrite, CompactMode::InPlace] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = sample_project(&tmp);
            let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
            let before = {
                let rx = cache.read().unwrap();
                (
                    rx.dump_graph().unwrap(),
                    raw_tables(&rx),
                    rx.links_all().unwrap(),
                )
            };
            let (cache, report) = cache.compact(&dir, mode, soon()).unwrap();
            assert_eq!(report.outcome, CompactOutcome::Done, "{mode:?}");
            assert_eq!(report.mode, mode);
            let rx = cache.read().unwrap();
            assert_eq!(rx.dump_graph().unwrap(), before.0, "{mode:?}");
            assert_eq!(raw_tables(&rx), before.1, "{mode:?}");
            assert_eq!(rx.links_all().unwrap(), before.2, "{mode:?}");
            drop(rx);
            near(
                cache.compacted_bytes().unwrap().unwrap(),
                cache.file_bytes().unwrap(),
            );
            assert!(!dir.cache_dir().join(katalogr_store::REWRITE_FILE).exists());
            // The compacted cache is a working cache: a reopen finds it current.
            drop(cache);
            let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
            assert_eq!(cache.report().replaced, 0);
        }
    }

    #[test]
    fn rewrite_shrinks_fragmented_file() {
        use katalogr_store::{CompactMode, CompactOutcome};
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        let mut cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        for round in 0..20 {
            for i in 0..25 {
                let n = round * 25 + i;
                dir.write_shard(&extract(
                    &format!("f{n:03}.tx"),
                    &format!(
                        "sym fn F{n}\nsym fn G{n}\nlink F{n} calls G{n}\nexport ns:k{n} F{n}\nref G{n} calls ns:k{}\n",
                        n / 2
                    ),
                ))
                .unwrap();
            }
            drop(cache);
            cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        }
        assert_eq!(cache.read().unwrap().artifacts().unwrap().len(), 500);
        let before_dump = cache.read().unwrap().dump_graph().unwrap();
        let before = cache.file_bytes().unwrap();
        let (cache, report) = cache.compact(&dir, CompactMode::Rewrite, soon()).unwrap();
        assert_eq!(report.outcome, CompactOutcome::Done);
        assert_eq!(report.bytes_before, before);
        assert_eq!(report.bytes_after, cache.file_bytes().unwrap());
        assert!(
            report.bytes_after < before,
            "{} < {before}",
            report.bytes_after
        );
        assert_eq!(cache.read().unwrap().dump_graph().unwrap(), before_dump);
    }

    #[test]
    fn rewrite_failure_keeps_cache() {
        use katalogr_store::{CompactMode, CompactOutcome};
        let tmp = tempfile::tempdir().unwrap();
        let dir = sample_project(&tmp);
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        let before = cache.read().unwrap().dump_graph().unwrap();
        let size = cache.file_bytes().unwrap();
        // A directory where the rewrite file goes: it cannot be removed or created.
        let blocker = dir.cache_dir().join(katalogr_store::REWRITE_FILE);
        std::fs::create_dir(&blocker).unwrap();
        let (cache, report) = cache.compact(&dir, CompactMode::Rewrite, soon()).unwrap();
        assert!(
            matches!(report.outcome, CompactOutcome::Skipped(_)),
            "{report:?}"
        );
        assert_eq!(cache.file_bytes().unwrap(), size);
        assert_eq!(cache.read().unwrap().dump_graph().unwrap(), before);
        assert!(blocker.is_dir(), "the blocker is left alone");
        // Still usable: it takes a write.
        let mut tx = cache.write().unwrap();
        tx.put_index_errors("r", &RelPath::new("x.tx").unwrap(), &[])
            .unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn orphan_rewrite_file_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = sample_project(&tmp);
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        drop(cache);
        let orphan = dir.cache_dir().join(katalogr_store::REWRITE_FILE);
        std::fs::write(&orphan, b"left by a process that stopped").unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert!(!orphan.exists());
        assert_eq!(cache.report().replaced, 0);
    }

    #[test]
    fn rebuild_from_shards_compacts() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = sample_project(&tmp);
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().replaced, 3);
        near(
            cache.compacted_bytes().unwrap().unwrap(),
            cache.file_bytes().unwrap(),
        );
        drop(cache);
        // A warm open leaves the file as it is.
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        let size = cache.file_bytes().unwrap();
        drop(cache);
        std::fs::remove_dir_all(dir.cache_dir()).unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().replaced, 3);
        let compacted = cache
            .compacted_bytes()
            .unwrap()
            .expect("compacted after the rebuild");
        near(compacted, cache.file_bytes().unwrap());
        assert!(compacted <= size + 4096 * 4, "{compacted} against {size}");
        assert!(!dir.cache_dir().join(katalogr_store::REWRITE_FILE).exists());
    }

    /// FEAT-080: a read-only handle opened before a rewrite goes on reading the file it opened,
    /// a complete older snapshot; a handle opened after sees the new file and what was written
    /// to it since. The rename never makes the old handle read garbage or fail.
    #[test]
    fn rewrite_beside_read_only_handle() {
        use katalogr_store::CompactMode;
        let tmp = tempfile::tempdir().unwrap();
        let dir = sample_project(&tmp);
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        let Some(old_reader) = Cache::open_reader(&dir).unwrap() else {
            // No byte-range locks on this platform: a reader waits for the writer instead.
            return;
        };
        assert!(old_reader.is_reader());
        let held = old_reader.read().unwrap();
        let graph = held.links_all().unwrap();
        assert!(!graph.is_empty());

        let (cache, report) = cache.compact(&dir, CompactMode::Rewrite, soon()).unwrap();
        assert_eq!(report.outcome, katalogr_store::CompactOutcome::Done);

        // The snapshot held across the swap, and a new one on the same handle, read as before.
        assert_eq!(held.links_all().unwrap(), graph);
        assert_eq!(old_reader.read().unwrap().links_all().unwrap(), graph);
        drop(held);
        // The old handle is never compacted.
        let (old_reader, skipped) = old_reader
            .compact(&dir, CompactMode::Rewrite, soon())
            .unwrap();
        assert!(matches!(
            skipped.outcome,
            katalogr_store::CompactOutcome::Skipped(_)
        ));
        assert!(old_reader.is_reader());

        // A write after the swap reaches handles opened after it, not the one on the old file.
        let mut tx = cache.write().unwrap();
        tx.put_index_errors(
            "r",
            &RelPath::new("x.tx").unwrap(),
            &[katalogr_store::ErrorRow {
                span: None,
                message: "after the swap".into(),
            }],
        )
        .unwrap();
        tx.commit().unwrap();
        let new_reader = Cache::open_reader(&dir)
            .unwrap()
            .expect("a reader opens the new file");
        assert_eq!(new_reader.read().unwrap().links_all().unwrap(), graph);
        assert_eq!(
            new_reader.read().unwrap().index_errors_all().unwrap().len(),
            1
        );
        assert!(
            old_reader
                .read()
                .unwrap()
                .index_errors_all()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn index_errors_all_sorted() {
        use katalogr_store::ErrorRow;
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        let row = |m: &str| ErrorRow {
            span: None,
            message: m.to_string(),
        };
        {
            let mut tx = cache.write().unwrap();
            tx.put_index_errors("r2", &RelPath::new("a.tx").unwrap(), &[row("x")])
                .unwrap();
            tx.put_index_errors("r1", &RelPath::new("z.tx").unwrap(), &[row("y"), row("z")])
                .unwrap();
            tx.put_index_errors("r1", &RelPath::new("b.tx").unwrap(), &[row("w")])
                .unwrap();
            tx.commit().unwrap();
        }
        let all = cache.read().unwrap().index_errors_all().unwrap();
        let keys: Vec<_> = all
            .iter()
            .map(|((r, p), rows)| (r.as_str(), p.as_str(), rows.len()))
            .collect();
        assert_eq!(
            keys,
            [("r1", "b.tx", 1), ("r1", "z.tx", 2), ("r2", "a.tx", 1)]
        );
    }

    #[test]
    fn busy_retries_until_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        let stitcher = Stitcher::new();
        let _held = Cache::open_or_rebuild(&dir, &stitcher, soon()).unwrap();

        let start = Instant::now();
        let second = Cache::open_or_rebuild(&dir, &stitcher, start + Duration::from_millis(200));
        let waited = start.elapsed();
        assert!(matches!(second, Err(StoreError::Busy)));
        assert!(
            waited >= Duration::from_millis(200),
            "returned after {waited:?}"
        );
        assert!(waited < Duration::from_secs(3), "returned after {waited:?}");
    }
}
