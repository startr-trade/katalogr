//! FEAT-076 R-6: the head of the rank is stored with the rank and read only while it is current.
//! (`session-start` over it is tested in `katalogr-dist/tests/rank_top.rs`.)

mod support;

use camino::Utf8PathBuf;
use katalogr_engine::Engine;
use katalogr_model::Id;
use katalogr_store::{RANK_CONTEXT, RANK_TOP_ENTRIES, Reader};
use support::{drained_graph, file_event, init_project, open_engine};

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
    fn publish(&self, text: &str) {
        std::fs::write(self.work.join("a.tx"), text).unwrap();
        self.engine.inbox().append(&file_event("a.tx")).unwrap();
        drained_graph(&self.engine);
    }
}

const BASE: &str =
    "sym fn a\nsym fn b\nsym fn c\nsym fn d\nlink a calls b\nlink c calls b\nlink b calls d\n";

mod rank_top {
    use super::*;

    #[test]
    fn stored_with_rank() {
        let f = fixture();
        f.publish(BASE);
        let rx = f.engine.cache().read().unwrap();
        assert!(rx.rank_is_current(RANK_CONTEXT).unwrap());
        let top = rx.rank_top().unwrap().expect("a stored top");
        assert_eq!(top.total, 4);
        // The head is the rank in `map` order, bit for bit.
        let live = katalogr_query::rank_live(&rx).unwrap();
        assert_eq!(top.entries.len(), live.len().min(RANK_TOP_ENTRIES));
        for ((id, score), (hit, want)) in top.entries.iter().zip(&live) {
            assert_eq!(*id, Id::of(&hit.name));
            assert_eq!(score.to_bits(), want.to_bits());
        }
        drop(rx);

        // A changed link recomputes both; a publish that changes no rank input keeps both.
        f.publish(&format!("{BASE}link d calls a\n"));
        let rx = f.engine.cache().read().unwrap();
        let second = rx.rank_top().unwrap().unwrap();
        assert_ne!(second, top);
        drop(rx);
        f.publish(&format!("{BASE}link d calls a\n"));
        let rx = f.engine.cache().read().unwrap();
        assert_eq!(rx.rank_top().unwrap().unwrap(), second);
        assert!(rx.rank_is_current(RANK_CONTEXT).unwrap());
    }

    #[test]
    fn not_current_uses_full_map() {
        let f = fixture();
        f.publish(BASE);
        let rx = f.engine.cache().read().unwrap();
        let (total, entries) = katalogr_query::map_top(&rx).unwrap().expect("current");
        assert_eq!(total, 4);
        let full = katalogr_query::map(&rx).unwrap();
        assert_eq!(entries.len(), full.len());
        for (a, b) in entries.iter().zip(&full) {
            assert_eq!((&a.symbol, a.rank), (&b.symbol, b.rank));
        }
        drop(rx);

        // A rank whose watermark is behind: the stored head is refused.
        let mut tx = f.engine.cache().write().unwrap();
        tx.set_rank_watermark(1_000_000).unwrap();
        tx.commit().unwrap();
        let rx = f.engine.cache().read().unwrap();
        assert!(rx.rank_top().unwrap().is_some(), "the blob is still there");
        assert!(katalogr_query::map_top(&rx).unwrap().is_none());
        // `map` still answers, from a live rank.
        assert_eq!(katalogr_query::map(&rx).unwrap().len(), 4);
    }
}
