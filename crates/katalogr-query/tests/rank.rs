//! FEAT-057: `rank` serves the stored rank when it reflects the watermark, else computes live.

use camino::Utf8PathBuf;
use katalogr_model::{
    ArtifactName, Confidence, Fragment, Id, LinkKind, Name, Provenance, RelPath, Segment, Span,
    SymbolName,
};
use katalogr_query::{rank, rank_cached_or_live, rank_live};
use katalogr_store::{
    Cache, Kindex, LinkEntry, LinkKey, LinkOrigin, LinkRow, ProjectConfig, ProjectName,
    RANK_CONTEXT, Reader, RepoConfig, Restitch, StoreError, SymbolRow, WriteTx, pagerank,
};
use std::time::{Duration, Instant};

struct NoShards;

impl Restitch for NoShards {
    fn replace_fragment(&self, _: &mut WriteTx, _: Fragment) -> Result<(), StoreError> {
        Ok(())
    }
    fn remove_artifact(&self, _: &mut WriteTx, _: &ArtifactName) -> Result<(), StoreError> {
        Ok(())
    }
}

mod rank {
    use super::*;

    fn graph(tx: &mut WriteTx) -> Vec<Id> {
        let path = RelPath::new("f.tx").unwrap();
        let artifact = ArtifactName {
            repo: "r".into(),
            path: path.clone(),
        };
        let provenance = Provenance {
            repo: "r".into(),
            path,
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        };
        let span = Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
        };
        let mut ids = Vec::new();
        for n in ["a", "b", "c", "d"] {
            let name = SymbolName {
                artifact: artifact.clone(),
                segments: vec![Segment {
                    kind: "fn".into(),
                    name: n.into(),
                }],
                ordinal: 1,
            };
            let id = name.id();
            tx.put_symbol(
                id,
                &SymbolRow {
                    name,
                    artifact: artifact.id(),
                    kind: "fn".into(),
                    span,
                    facts: Default::default(),
                },
            )
            .unwrap();
            ids.push(id);
        }
        for (from, to, fanout) in [(0, 1, None), (2, 1, None), (3, 0, Some(4))] {
            tx.put_link(
                &LinkKey {
                    from: ids[from],
                    kind: LinkKind("calls".into()),
                    to: ids[to],
                },
                &LinkRow {
                    entries: vec![LinkEntry {
                        origin: LinkOrigin::Intra,
                        confidence: if fanout.is_some() {
                            Confidence::Inferred
                        } else {
                            Confidence::Extracted
                        },
                        span: Some(span),
                        provenance: provenance.clone(),
                        fanout,
                        produced_by: None,
                    }],
                },
            )
            .unwrap();
        }
        ids
    }

    #[test]
    fn served_from_cache() {
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
        let cache =
            Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60))
                .unwrap();
        let mut tx = cache.write().unwrap();
        let ids = graph(&mut tx);
        tx.commit().unwrap();

        // Nothing stored: computed live.
        let live = rank_live(&cache.read().unwrap()).unwrap();
        assert_eq!(rank(&cache.read().unwrap()).unwrap(), live);

        // Stored at the current watermark: the same answer, bit for bit.
        let mut tx = cache.write().unwrap();
        let symbols = tx.symbols().unwrap();
        let scores = pagerank(&tx, &symbols).unwrap();
        let stored: Vec<(Id, f64)> = symbols.iter().map(|(i, _)| *i).zip(scores).collect();
        let wm = tx.watermark(RANK_CONTEXT).unwrap();
        tx.store_rank(&stored, wm).unwrap();
        tx.commit().unwrap();
        assert_eq!(rank_cached_or_live(&cache.read().unwrap()).unwrap(), live);

        // Planted values prove the stored rank is what is served.
        let planted: Vec<(Id, f64)> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, 10.0 + i as f64))
            .collect();
        let mut tx = cache.write().unwrap();
        tx.store_rank(&planted, wm).unwrap();
        tx.commit().unwrap();
        let served = rank(&cache.read().unwrap()).unwrap();
        assert_eq!(served[0].1, 13.0);
        assert_ne!(served, live);

        // Behind the watermark: ignored, computed live again.
        let mut tx = cache.write().unwrap();
        tx.set_last_seq("worktree", 7).unwrap();
        tx.set_watermark("worktree", 7).unwrap();
        tx.commit().unwrap();
        assert_eq!(rank(&cache.read().unwrap()).unwrap(), live);

        // A write that changes the graph drops the stored rank.
        let mut tx = cache.write().unwrap();
        tx.store_rank(&planted, 7).unwrap();
        tx.commit().unwrap();
        assert_ne!(rank(&cache.read().unwrap()).unwrap(), live);
        let mut tx = cache.write().unwrap();
        tx.remove_symbol(ids[3]).unwrap();
        tx.commit().unwrap();
        let r = cache.read().unwrap();
        assert_eq!(r.cached_rank(RANK_CONTEXT).unwrap(), None);
        assert_eq!(rank(&r).unwrap().len(), 3);
    }
}
