//! FEAT-077: multi-target inferred resolutions are stored once as fan rows and read back as
//! the per-target links they stand for.
mod support;

use camino::Utf8PathBuf;
use katalogr_engine::{Stitcher, canonical_graph};
use katalogr_model::{
    Confidence, ExportKey, ExtractInput, Extractor, Fragment, Id, LinkKind, Name, RelPath,
};
use katalogr_store::{
    Cache, FanKey, Kindex, LinkKey, LinkOrigin, ProjectConfig, ProjectName, Reader, RepoConfig,
};
use std::time::{Duration, Instant};
use support::TestExtractor;

struct Env {
    _tmp: tempfile::TempDir,
    cache: Cache,
    stitcher: Stitcher,
}

fn env() -> Env {
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
        Cache::open_or_rebuild(&dir, &stitcher, Instant::now() + Duration::from_secs(5)).unwrap();
    Env {
        _tmp: tmp,
        cache,
        stitcher,
    }
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

impl Env {
    fn put(&self, path: &str, text: &str) -> katalogr_engine::StitchDelta {
        let mut tx = self.cache.write().unwrap();
        let d = self
            .stitcher
            .replace_fragment(&mut tx, extract(path, text))
            .unwrap();
        tx.commit().unwrap();
        d
    }

    fn remove(&self, path: &str) -> katalogr_engine::StitchDelta {
        let mut tx = self.cache.write().unwrap();
        let name = katalogr_model::ArtifactName::new("r", path).unwrap();
        let d = self.stitcher.remove_artifact(&mut tx, &name).unwrap();
        tx.commit().unwrap();
        d
    }

    fn fans(&self) -> Vec<FanKey> {
        self.cache
            .read()
            .unwrap()
            .fans_all()
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect()
    }

    fn graph(&self) -> String {
        canonical_graph(&self.cache.read().unwrap())
    }
}

fn sym(path: &str, name: &str) -> Id {
    extract(path, &format!("sym fn {name}\n")).symbols[0]
        .name
        .id()
}

fn k(key: &str) -> ExportKey {
    ExportKey {
        namespace: "ns".into(),
        key: key.into(),
    }
}

/// Three exporters of `ns:k` and a file `z.tx` whose `Z` calls it.
fn three(e: &Env) {
    e.put("x.tx", "sym fn X\nexport ns:k X\n");
    e.put("y.tx", "sym fn Y\nexport ns:k Y\n");
    e.put("w.tx", "sym fn W\nexport ns:k W\n");
    e.put("z.tx", "sym fn Z\nref Z calls ns:k\n");
}

mod fanout {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn multi_target_ref_stored_once() {
        let e = env();
        three(&e);
        let z = sym("z.tx", "Z");
        assert_eq!(e.fans().len(), 1);
        let rx = e.cache.read().unwrap();
        assert!(rx.stored_links_out(z, None).unwrap().is_empty());
        let out = rx.links_out(z, None).unwrap();
        assert_eq!(out.len(), 3);
        for (key, row) in &out {
            assert_eq!(key.kind.0, "calls");
            assert_eq!(row.entries.len(), 1);
            let en = &row.entries[0];
            assert_eq!(en.confidence, Confidence::Inferred);
            assert_eq!(en.fanout, Some(3));
            assert_eq!(en.origin, LinkOrigin::Ref(0));
        }
    }

    #[test]
    fn fan_rows_track_export_changes() {
        let e = env();
        three(&e);
        let z = sym("z.tx", "Z");
        let before = e.cache.read().unwrap().fans_all().unwrap();
        // A fourth exporter: the fan row is not rewritten, the expansion grows.
        let d = e.put("v.tx", "sym fn V\nexport ns:k V\n");
        assert!(d.fans_added.is_empty() && d.fans_removed.is_empty());
        let rx = e.cache.read().unwrap();
        assert_eq!(rx.fans_all().unwrap(), before);
        let out = rx.links_out(z, None).unwrap();
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|(_, r)| r.entries[0].fanout == Some(4)));
        drop(rx);
        e.remove("v.tx");
        let rx = e.cache.read().unwrap();
        assert_eq!(rx.links_out(z, None).unwrap().len(), 3);
        assert_eq!(rx.fans_all().unwrap(), before);
    }

    #[test]
    fn fan_to_single_restores_confidence() {
        let e = env();
        three(&e);
        let z = sym("z.tx", "Z");
        e.remove("x.tx");
        assert_eq!(e.fans().len(), 1);
        e.remove("y.tx");
        assert!(e.fans().is_empty());
        let rx = e.cache.read().unwrap();
        let out = rx.stored_links_out(z, None).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.entries[0].confidence, Confidence::Extracted);
        assert_eq!(out[0].1.entries[0].fanout, None);
        assert_eq!(out[0].0.to, sym("w.tx", "W"));
        drop(rx);
        // And back to a fan row when a second exporter returns.
        e.put("x.tx", "sym fn X\nexport ns:k X\n");
        assert_eq!(e.fans().len(), 1);
        assert!(
            e.cache
                .read()
                .unwrap()
                .stored_links_out(z, None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn fan_to_none_unresolved() {
        let e = env();
        three(&e);
        let z = sym("z.tx", "Z");
        for p in ["x.tx", "y.tx", "w.tx"] {
            e.remove(p);
        }
        assert!(e.fans().is_empty());
        let rx = e.cache.read().unwrap();
        assert!(rx.links_out(z, None).unwrap().is_empty());
        assert_eq!(rx.unresolved_refs(None).unwrap().len(), 1);
    }

    #[test]
    fn candidate_fallback_moves_fan_row() {
        let e = env();
        e.put("x.tx", "sym fn X\nexport ns:k X\n");
        e.put("y.tx", "sym fn Y\nexport ns:k Y\n");
        e.put("z.tx", "sym fn Z\nrefs Z calls ns:a ns:k\n");
        let z = sym("z.tx", "Z");
        assert_eq!(e.fans()[0].key, k("k"));
        // An earlier candidate gains one export: a stored link, no fan row.
        e.put("a.tx", "sym fn A\nexport ns:a A\n");
        assert!(e.fans().is_empty());
        let rx = e.cache.read().unwrap();
        let out = rx.links_out(z, None).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.to, sym("a.tx", "A"));
        assert_eq!(out[0].1.entries[0].confidence, Confidence::Extracted);
        drop(rx);
        // Two exports on the earlier candidate: the fan row moves to it.
        e.put("b.tx", "sym fn B\nexport ns:a B\n");
        assert_eq!(e.fans().len(), 1);
        assert_eq!(e.fans()[0].key, k("a"));
        // It leaves again when the earlier candidate is emptied.
        e.remove("a.tx");
        e.remove("b.tx");
        assert_eq!(e.fans()[0].key, k("k"));
    }

    #[test]
    fn replace_and_remove_drop_fan_rows() {
        let e = env();
        three(&e);
        assert_eq!(e.fans().len(), 1);
        e.put("z.tx", "sym fn Z\n");
        assert!(e.fans().is_empty());
        let rx = e.cache.read().unwrap();
        assert!(
            rx.raw_scan(katalogr_store::cache::TableId::FanIn, &[])
                .unwrap()
                .is_empty()
        );
        drop(rx);
        e.put("z.tx", "sym fn Z\nref Z calls ns:k\n");
        assert_eq!(e.fans().len(), 1);
        e.remove("z.tx");
        assert!(e.fans().is_empty());
        let rx = e.cache.read().unwrap();
        assert!(
            rx.raw_scan(katalogr_store::cache::TableId::FanIn, &[])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rebuild_equals_incremental() {
        use support::{cache_dump, drained_graph, init_project, open_engine};
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let kx = root.join("kx");
        init_project(&kx);
        let files = [
            ("x.tx", "sym fn X\nexport ns:k X\n"),
            ("y.tx", "sym fn Y\nexport ns:k Y\nexport ns:j Y\n"),
            ("w.tx", "sym fn W\nexport ns:k W\nexport ns:j W\n"),
            (
                "z.tx",
                "sym fn Z\nrefs Z calls ns:j ns:k\nref Z uses ns:k\n",
            ),
            ("q.tx", "sym fn Q\nref Q calls ns:k\n"),
        ];
        let e = open_engine(&kx, &work);
        for (p, t) in files {
            std::fs::write(work.join(p), t).unwrap();
            e.inbox().append(&support::file_event(p)).unwrap();
            drained_graph(&e);
        }
        std::fs::write(work.join("q.tx"), "sym fn Q\nref Q calls ns:j\n").unwrap();
        e.inbox().append(&support::file_event("q.tx")).unwrap();
        let g1 = drained_graph(&e);
        let d1 = cache_dump(&e);
        assert!(
            d1.iter()
                .any(|(n, rows)| n == "fan_out" && !rows.is_empty())
        );
        drop(e);
        std::fs::remove_file(kx.join("projects/p/.cache/index.redb")).unwrap();
        let e = open_engine(&kx, &work);
        let g2 = drained_graph(&e);
        assert_eq!(g1, g2);
        let d2 = cache_dump(&e);
        for ((n1, r1), (_, r2)) in d1.iter().zip(&d2) {
            // The shard-hash stat (size, mtime) is per cache file, not graph content.
            if n1 != "shard_hashes" {
                assert_eq!(r1, r2, "table {n1}");
            }
        }
    }

    const CONTENTS: [&str; 8] = [
        "sym fn S\nexport ns:k S\n",
        "sym fn S\nexport ns:k S\nexport ns:j S\n",
        "sym fn S\nref S calls ns:k\n",
        "sym fn S\nrefs S calls ns:j ns:k\n",
        "sym fn S\nexport ns:j S\nref S calls ns:k\n",
        "sym fn S\nsym fn T\nref S calls ns:k\nrefs T uses ns:k ns:j\nexport ns:j T\n",
        "sym fn S\nlink S calls S\n",
        "sym fn S\nrefs S calls ns:k ns:j\nref S calls ns:j\n",
    ];
    const PATHS: [&str; 6] = ["a.tx", "b.tx", "c.tx", "d.tx", "e.tx", "f.tx"];

    /// Invariants 1 to 3 of the Design over the current cache.
    fn check_invariants(e: &Env) {
        let rx = e.cache.read().unwrap();
        for (fk, row) in rx.fans_all().unwrap() {
            let targets = rx.exports(&fk.key).unwrap();
            assert!(targets.len() >= 2, "invariant 1: {fk:?}");
            let aid = rx.symbol(fk.from).unwrap().map_or(fk.from, |s| s.artifact);
            let art = rx.artifact(aid).unwrap().expect("artifact of a fan row");
            for en in &row.entries {
                let LinkOrigin::Ref(i) = en.origin else {
                    panic!("fan entry with origin {:?}", en.origin)
                };
                let r = &art.refs[i as usize];
                assert_eq!((r.from, &r.kind), (fk.from, &fk.kind));
                let win = r
                    .candidates
                    .iter()
                    .find(|c| !rx.exports(c).unwrap().is_empty())
                    .expect("winning key");
                assert_eq!(*win, fk.key, "invariant 2");
                for t in &targets {
                    let stored = rx
                        .stored_link(&LinkKey {
                            from: fk.from,
                            kind: fk.kind.clone(),
                            to: *t,
                        })
                        .unwrap();
                    assert!(
                        stored.is_none_or(|s| s.entries.iter().all(|x| x.origin != en.origin)),
                        "invariant 3"
                    );
                }
            }
        }
    }

    fn step() -> impl Strategy<Value = (usize, Option<usize>)> {
        (
            0..PATHS.len(),
            prop::option::weighted(0.8, 0..CONTENTS.len()),
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn threshold_invariant_graph(steps in prop::collection::vec(step(), 1..14)) {
            let (fan, flat) = (env(), env());
            flat.stitcher.set_fan_rows_from(u32::MAX);
            for (p, c) in steps {
                for e in [&fan, &flat] {
                    match c {
                        Some(c) => { e.put(PATHS[p], CONTENTS[c]); }
                        None => { e.remove(PATHS[p]); }
                    }
                }
                prop_assert_eq!(fan.graph(), flat.graph());
                prop_assert_eq!(
                    fan.cache.read().unwrap().links_all().unwrap(),
                    flat.cache.read().unwrap().links_all().unwrap()
                );
                check_invariants(&fan);
                prop_assert!(flat.fans().is_empty());
            }
        }
    }

    #[allow(dead_code)]
    fn unused(_: LinkKind) {}
}
