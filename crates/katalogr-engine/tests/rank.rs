//! FEAT-057: the rank is stored at publish and kept when a publish changes no rank input.

mod support;

use camino::Utf8PathBuf;
use katalogr_engine::Engine;
use katalogr_store::{RANK_CONTEXT, Reader, pagerank};
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
}
