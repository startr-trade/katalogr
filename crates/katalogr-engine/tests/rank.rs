//! FEAT-057: the rank is stored at publish and kept when a publish changes no rank input.

mod support;

use camino::Utf8PathBuf;
use katalogr_engine::queue::Processor;
use katalogr_engine::{Consistency, Engine, QueryOptions, Scope};
use katalogr_store::{RANK_CONTEXT, Reader, pagerank};
use std::time::{Duration, Instant};
use support::{
    cache_dump, drained_graph, file_event, full_index, init_project, open_engine,
    open_engine_threads, shard_bytes,
};

struct Fixture {
    _tmp: tempfile::TempDir,
    work: Utf8PathBuf,
    engine: Engine,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
    let work = Utf8PathBuf::from_path_buf(tmp.path().join("work")).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    init_project(&kx);
    let engine = open_engine(&kx, &work);
    Fixture {
        _tmp: tmp,
        work,
        engine,
    }
}

impl Fixture {
    /// Write `text` to `a.tx`, publish it and wait for the queue to drain.
    fn publish(&self, text: &str) {
        std::fs::write(self.work.join("a.tx"), text).unwrap();
        self.engine.inbox().append(&file_event("a.tx")).unwrap();
        drained_graph(&self.engine);
    }

    /// The stored rank, when it reflects the current watermark.
    fn stored(&self) -> Option<Vec<(katalogr_model::Id, f64)>> {
        let r = self.engine.cache().read().unwrap();
        r.cached_rank(RANK_CONTEXT)
            .unwrap()
            .map(|m| m.into_iter().collect())
    }

    /// The rank computed now, in the order `stored` returns (by id).
    fn live(&self) -> Vec<(katalogr_model::Id, f64)> {
        let r = self.engine.cache().read().unwrap();
        let symbols = r.symbols().unwrap();
        let scores = pagerank(&r, &symbols).unwrap();
        let mut v: Vec<_> = symbols.iter().map(|(id, _)| *id).zip(scores).collect();
        v.sort_by_key(|(id, _)| *id);
        v
    }
}

mod rank {
    use super::*;

    const BASE: &str = "sym fn a\nsym fn b\nsym fn c\nlink a calls b\nlink c calls b\n";

    #[test]
    fn computed_at_publish() {
        let f = fixture();
        assert_eq!(f.stored(), None, "nothing stored before the first publish");
        f.publish(BASE);
        let first = f.stored().expect("rank stored with the current watermark");
        assert_eq!(first, f.live(), "stored rank is exactly the live rank");
        {
            let r = f.engine.cache().read().unwrap();
            assert_eq!(
                r.meta("rank_watermark").unwrap().unwrap(),
                r.watermark(RANK_CONTEXT).unwrap().to_le_bytes()
            );
        }

        // A changed link recomputes it.
        f.publish(&format!("{BASE}link b calls a\n"));
        let second = f.stored().expect("rank stored after the link change");
        assert_eq!(second, f.live());
        assert_ne!(first, second);

        // So does a new symbol with no link: the teleport share of every symbol changes.
        f.publish(&format!("{BASE}link b calls a\nsym fn d\n"));
        let third = f.stored().expect("rank stored after the symbol change");
        assert_eq!(third.len(), 4);
        assert_eq!(third, f.live());
    }

    #[test]
    fn unchanged_links_skip() {
        let f = fixture();
        f.publish(BASE);
        let before = f.stored().unwrap();
        // Plant a marker value under the current watermark; a recompute would overwrite it.
        let marker: Vec<_> = before.iter().map(|(id, _)| (*id, -1.0)).collect();
        {
            let mut tx = f.engine.cache().write().unwrap();
            let wm = tx.watermark(RANK_CONTEXT).unwrap();
            tx.store_rank(&marker, wm).unwrap();
            tx.commit().unwrap();
        }
        let wm_before = f
            .engine
            .cache()
            .read()
            .unwrap()
            .watermark(RANK_CONTEXT)
            .unwrap();

        // A publish that changes the file but no symbol and no link.
        f.publish(&format!("# a comment\n{BASE}"));
        let wm_after = f
            .engine
            .cache()
            .read()
            .unwrap()
            .watermark(RANK_CONTEXT)
            .unwrap();
        assert!(wm_after > wm_before, "the publish advanced the watermark");
        assert_eq!(
            f.stored().expect("rank still current"),
            marker,
            "kept, not recomputed, and valid at the new watermark"
        );
    }

    #[test]
    fn rank_context_is_the_worktree_context() {
        assert_eq!(RANK_CONTEXT, katalogr_engine::queue::Context::WORKTREE);
    }

    use katalogr_engine::NewEvent;

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    /// Write `n` files that each define a symbol and link it to the first file's symbol.
    fn write_files(work: &camino::Utf8Path, n: usize) {
        for i in 0..n {
            let text = if i == 0 {
                "sym fn S0\nexport ns:hub S0\n".to_string()
            } else {
                format!("sym fn S{i}\nref S{i} calls ns:hub\n")
            };
            std::fs::write(work.join(format!("f{i}.tx")), text).unwrap();
        }
    }

    fn pending(e: &Engine) -> usize {
        e.cache()
            .read()
            .unwrap()
            .pending_entries(None)
            .unwrap()
            .len()
    }

    fn current(e: &Engine) -> bool {
        e.cache()
            .read()
            .unwrap()
            .rank_is_current(RANK_CONTEXT)
            .unwrap()
    }

    /// Index everything under the repository root with rounds of `budget` paths.
    fn drain_with_budget(e: &Engine, budget: usize) {
        e.inbox().append(&NewEvent::rescan("r")).unwrap();
        let mut p = Processor::try_acquire(e).unwrap().unwrap();
        p.ingest().unwrap();
        while pending(e) > 0 {
            p.process(&Scope::Project, budget, soon()).unwrap();
        }
    }

    /// FEAT-093 R-4. 600 files take three rounds and one rank computation.
    #[test]
    fn refreshed_once_per_drain() {
        let f = fixture();
        write_files(&f.work, 600);
        full_index(&f.engine);
        let stats = f.engine.index_stats();
        assert_eq!(stats.files, 600);
        assert_eq!(stats.rounds, 3);
        assert_eq!(stats.rank_refreshes, 1);
        assert!(current(&f.engine));
    }

    /// FEAT-093 R-4. A round that leaves paths pending does not compute the rank; the round
    /// that empties the queue does.
    #[test]
    fn budget_cut_leaves_rank_for_the_last_round() {
        let f = fixture();
        write_files(&f.work, 3);
        f.engine.inbox().append(&NewEvent::rescan("r")).unwrap();
        let mut p = Processor::try_acquire(&f.engine).unwrap().unwrap();
        p.ingest().unwrap();
        for left in [2, 1] {
            p.process(&Scope::Project, 1, soon()).unwrap();
            assert_eq!(pending(&f.engine), left);
            assert!(!current(&f.engine), "not current with {left} pending");
        }
        p.process(&Scope::Project, 1, soon()).unwrap();
        assert_eq!(pending(&f.engine), 0);
        assert!(current(&f.engine));
        assert_eq!(f.engine.index_stats().rank_refreshes, 1);
    }

    fn snapshot(e: &Engine, kx: &camino::Utf8Path) -> (support::CacheDump, Vec<(String, Vec<u8>)>) {
        let dump = cache_dump(e)
            .into_iter()
            .filter(|(n, _)| n != "shard_hashes")
            .collect();
        (dump, shard_bytes(kx).into_iter().collect())
    }

    /// FEAT-093 R-16. The round size and the thread count change neither the shards nor any
    /// cache table, the rank included; a publish that changes no rank input keeps a rank equal
    /// to a fresh index's.
    #[test]
    fn rounds_never_change_final_results() {
        let mut reference = None;
        for threads in [1, 8] {
            for budget in [1, 7, 256] {
                let tmp = tempfile::tempdir().unwrap();
                let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
                let work = Utf8PathBuf::from_path_buf(tmp.path().join("work")).unwrap();
                std::fs::create_dir_all(&work).unwrap();
                init_project(&kx);
                write_files(&work, 40);
                let e = open_engine_threads(&kx, &work, threads);
                drain_with_budget(&e, budget);
                assert!(current(&e));
                let snap = snapshot(&e, &kx);
                assert!(
                    snap.0
                        .iter()
                        .any(|(n, rows)| n == "rank" && !rows.is_empty()),
                    "the rank is in the dump"
                );
                match &reference {
                    None => reference = Some(snap),
                    Some(r) => assert!(*r == snap, "budget {budget}, {threads} threads"),
                }
            }
        }

        // A publish that changes no rank input keeps the stored rank, equal to a fresh index.
        let tmp = tempfile::tempdir().unwrap();
        let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let work = Utf8PathBuf::from_path_buf(tmp.path().join("work")).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        init_project(&kx);
        write_files(&work, 12);
        let e = open_engine(&kx, &work);
        drain_with_budget(&e, 5);
        let refreshes = e.index_stats().rank_refreshes;
        let text = std::fs::read_to_string(work.join("f3.tx")).unwrap();
        std::fs::write(work.join("f3.tx"), format!("# a comment\n{text}")).unwrap();
        e.inbox().append(&file_event("f3.tx")).unwrap();
        drained_graph(&e);
        assert_eq!(
            e.index_stats().rank_refreshes,
            refreshes,
            "kept, not recomputed"
        );
        assert!(current(&e));

        let tmp2 = tempfile::tempdir().unwrap();
        let kx2 = Utf8PathBuf::from_path_buf(tmp2.path().join("kx")).unwrap();
        init_project(&kx2);
        let fresh = open_engine(&kx2, &work);
        full_index(&fresh);
        let a: Vec<_> = cache_dump(&e)
            .into_iter()
            .filter(|(n, _)| n == "rank")
            .collect();
        let b: Vec<_> = cache_dump(&fresh)
            .into_iter()
            .filter(|(n, _)| n == "rank")
            .collect();
        assert!(!a[0].1.is_empty() && a == b);
    }

    /// FEAT-093 R-16. The stored rank is served only while it reflects the snapshot.
    #[test]
    fn reader_uses_stored_rank_only_while_current() {
        let f = fixture();
        write_files(&f.work, 4);
        full_index(&f.engine);
        {
            let r = f.engine.cache().read().unwrap();
            let (_, cached) = katalogr_query::rank_cached_or_live_with_source(&r).unwrap();
            assert!(cached, "after a drain the stored rank is served");
        }
        // A commit that leaves a path pending: the rank is behind, so the reader computes live.
        std::fs::write(
            f.work.join("f1.tx"),
            "sym fn S1\nsym fn S9\nref S9 calls ns:hub\n",
        )
        .unwrap();
        std::fs::write(f.work.join("f2.tx"), "sym fn S2\n").unwrap();
        f.engine.inbox().append(&file_event("f1.tx")).unwrap();
        f.engine.inbox().append(&file_event("f2.tx")).unwrap();
        let mut p = Processor::try_acquire(&f.engine).unwrap().unwrap();
        p.ingest().unwrap();
        p.process(&Scope::Project, 1, soon()).unwrap();
        assert_eq!(pending(&f.engine), 1);
        let r = f.engine.cache().read().unwrap();
        let (live, cached) = katalogr_query::rank_cached_or_live_with_source(&r).unwrap();
        assert!(!cached);
        let again = katalogr_query::rank_live(&r).unwrap();
        assert_eq!(
            live.iter().map(|(_, s)| s.to_bits()).collect::<Vec<_>>(),
            again.iter().map(|(_, s)| s.to_bits()).collect::<Vec<_>>()
        );
    }

    /// FEAT-093 R-16. A `stale-ok` read during a drain computes the rank live and is marked
    /// stale.
    #[test]
    fn stale_reader_computes_live_during_drain() {
        let f = fixture();
        write_files(&f.work, 4);
        f.engine.inbox().append(&NewEvent::rescan("r")).unwrap();
        let mut p = Processor::try_acquire(&f.engine).unwrap().unwrap();
        p.ingest().unwrap();
        p.process(&Scope::Project, 2, soon()).unwrap();
        drop(p);
        let o = QueryOptions {
            consistency: Consistency::StaleOk,
            timeout: Duration::from_secs(5),
            scope: Scope::Project,
        };
        let a = f
            .engine
            .with_consistency(&o, |tx| {
                let (ranked, cached) = katalogr_query::rank_cached_or_live_with_source(tx).unwrap();
                let live = katalogr_query::rank_live(tx).unwrap();
                (ranked, cached, live)
            })
            .unwrap();
        assert!(a.stale && a.pending == 2);
        let (ranked, cached, live) = a.value;
        assert!(!cached && !ranked.is_empty());
        assert_eq!(
            ranked.iter().map(|(_, s)| s.to_bits()).collect::<Vec<_>>(),
            live.iter().map(|(_, s)| s.to_bits()).collect::<Vec<_>>()
        );
    }

    use katalogr_model::Id;
    use katalogr_store::{link_weight, row_weight};
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    /// The rank before ADR-0033, kept as a reference: one edge per expanded `links_all` row.
    fn expanded_pagerank(
        tx: &impl Reader,
        symbols: &[(Id, katalogr_store::SymbolRow)],
    ) -> Vec<f64> {
        let _ = link_weight;
        let n = symbols.len();
        if n == 0 {
            return Vec::new();
        }
        let index: BTreeMap<Id, usize> = symbols
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (*id, i))
            .collect();
        let mut edges: Vec<(usize, usize, f64)> = Vec::new();
        let mut out_weight = vec![0.0f64; n];
        for (key, row) in tx.links_all().unwrap() {
            let (Some(&a), Some(&b)) = (index.get(&key.from), index.get(&key.to)) else {
                continue;
            };
            if a == b {
                continue;
            }
            let w = row_weight(&row);
            if w <= 0.0 {
                continue;
            }
            out_weight[a] += w;
            edges.push((a, b, w));
        }
        let nf = n as f64;
        let mut r = vec![1.0 / nf; n];
        for _ in 0..katalogr_store::PAGERANK_ITERATIONS {
            let dangling: f64 = (0..n).filter(|i| out_weight[*i] == 0.0).map(|i| r[i]).sum();
            let base = (1.0 - katalogr_store::PAGERANK_DAMPING) / nf
                + katalogr_store::PAGERANK_DAMPING * dangling / nf;
            let mut next = vec![base; n];
            for &(a, b, w) in &edges {
                next[b] += katalogr_store::PAGERANK_DAMPING * r[a] * w / out_weight[a];
            }
            r = next;
        }
        r
    }

    const CONTENTS: [&str; 9] = [
        "sym fn S\nexport k:a S\n",
        "sym fn S\nexport k:a S\nexport k:b S\n",
        "sym fn S\nref S calls k:a\n",
        "sym fn S\nrefs S calls k:b k:a\nref S uses k:a\n",
        "sym fn S\nsym fn T\nexport k:b T\nrefs S calls k:a k:b\nlink S calls T\n",
        "sym fn S\nref S calls k:a\nlink S calls S\n",
        "sym fn S\nlink S uses T\nsym fn T\nref T uses k:b\n",
        "sym fn S\nboundref S consumes ch:x k:a\n",
        "sym fn S\n",
    ];
    const PATHS: [&str; 6] = ["a.tx", "b.tx", "c.tx", "d.tx", "e.tx", "f.tx"];

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// FEAT-093 R-5. Edges built from fan rows give the bit-identical scores of the
        /// expanded graph: stored links, fan rows, overlaps of both, two kinds and targets that
        /// are not symbols (file exports).
        #[test]
        fn compact_edges_match_expansion(
            files in prop::collection::vec(
                (0..PATHS.len(), 0..CONTENTS.len()),
                1..12,
            ),
        ) {
            let tmp = tempfile::tempdir().unwrap();
            let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
            let work = Utf8PathBuf::from_path_buf(tmp.path().join("work")).unwrap();
            std::fs::create_dir_all(&work).unwrap();
            init_project(&kx);
            for (p, c) in &files {
                std::fs::write(work.join(PATHS[*p]), CONTENTS[*c]).unwrap();
            }
            let e = open_engine(&kx, &work);
            full_index(&e);
            let r = e.cache().read().unwrap();
            let symbols = r.symbols().unwrap();
            let new: Vec<u64> = pagerank(&r, &symbols).unwrap().iter().map(|x| x.to_bits()).collect();
            let old: Vec<u64> = expanded_pagerank(&r, &symbols).iter().map(|x| x.to_bits()).collect();
            prop_assert_eq!(new, old);
        }
    }
}
