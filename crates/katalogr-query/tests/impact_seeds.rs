mod support;

mod impact_seeds {
    use super::support::*;
    use katalogr_model::annotate::ITEM_NS;
    use katalogr_model::{Confidence, ExportKey, Id, LinkKind, Name, OutRef, RelPath};
    use katalogr_query::{
        ChangedFile, SEED_BROKEN_REF, SEED_FILE, SEED_ITEM, SEED_SYMBOL, Seed, seeds_for_changes,
        seeds_for_file, seeds_for_item, seeds_for_symbol,
    };
    use katalogr_store::{Cache, RefId, WriteTx};

    const R: &str = "r";

    fn change(path: &str, kind: &str, ranges: &[(u32, u32)]) -> ChangedFile {
        ChangedFile {
            path: RelPath::new(path).unwrap(),
            kind: kind.into(),
            ranges: ranges.to_vec(),
        }
    }

    fn build(f: impl FnOnce(&mut WriteTx)) -> (tempfile::TempDir, Cache) {
        let tmp = tempfile::tempdir().unwrap();
        let cache = open(&tmp, &[R]);
        {
            let mut tx = cache.write().unwrap();
            f(&mut tx);
            tx.commit().unwrap();
        }
        (tmp, cache)
    }

    fn shape(seeds: &[Seed]) -> Vec<(Id, &'static str)> {
        let mut v: Vec<_> = seeds.iter().map(|s| (s.id, s.precision)).collect();
        v.sort();
        v
    }

    fn sorted(mut v: Vec<(Id, &'static str)>) -> Vec<(Id, &'static str)> {
        v.sort();
        v
    }

    #[test]
    fn hunks_seed_innermost_symbol() {
        let (_t, c) = build(|tx| {
            artifact(
                tx,
                R,
                "code.tx",
                &[("cls", 1, 30), ("m1", 2, 10), ("m2", 12, 20)],
            );
        });
        let tx = c.read().unwrap();
        let id = |n: &str| Id::of(&format!("r:code.tx#fn:{n}"));
        // A change in a method seeds the method, not its container.
        let s = seeds_for_changes(&tx, R, &[change("code.tx", "modified", &[(3, 4)])]).unwrap();
        assert_eq!(shape(&s), [(id("m1"), SEED_SYMBOL)]);
        // A change spanning two methods and the lines between seeds both and the class.
        let s = seeds_for_changes(&tx, R, &[change("code.tx", "modified", &[(8, 14)])]).unwrap();
        assert_eq!(
            shape(&s),
            sorted(vec![
                (id("cls"), SEED_SYMBOL),
                (id("m1"), SEED_SYMBOL),
                (id("m2"), SEED_SYMBOL)
            ])
        );
        // A symbol seed, a file seed and an unknown file.
        assert_eq!(
            seeds_for_symbol(&katalogr_query::result::SymbolHit {
                name: "r:code.tx#fn:m1".into(),
                kind: "fn".into(),
                path: "code.tx".into(),
                span: katalogr_model::Span {
                    start_line: 2,
                    start_col: 1,
                    end_line: 10,
                    end_col: 1
                },
            }),
            [Seed {
                id: id("m1"),
                precision: SEED_SYMBOL,
                wanted: None
            }]
        );
        let f = seeds_for_file(&tx, R, &RelPath::new("code.tx").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(f.len(), 4);
        assert!(f.iter().all(|s| s.precision == SEED_FILE));
        assert!(
            seeds_for_file(&tx, R, &RelPath::new("nope.tx").unwrap())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn hunk_outside_symbols_seeds_file() {
        let (_t, c) = build(|tx| {
            artifact(tx, R, "code.tx", &[("fn_a", 1, 5), ("fn_b", 10, 20)]);
            artifact(tx, R, "new.tx", &[("fresh", 1, 3)]);
        });
        let tx = c.read().unwrap();
        let file = katalogr_model::ArtifactName {
            repo: R.into(),
            path: RelPath::new("code.tx").unwrap(),
        }
        .id();
        let s = seeds_for_changes(&tx, R, &[change("code.tx", "modified", &[(7, 8)])]).unwrap();
        assert_eq!(shape(&s), [(file, SEED_FILE)]);
        // No changed line at all: still the file.
        let s = seeds_for_changes(&tx, R, &[change("code.tx", "modified", &[])]).unwrap();
        assert_eq!(shape(&s), [(file, SEED_FILE)]);
        // An added file seeds its artifact and symbols at `file`.
        let s = seeds_for_changes(&tx, R, &[change("new.tx", "added", &[])]).unwrap();
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|s| s.precision == SEED_FILE));
        // A file the graph does not hold seeds nothing.
        let s = seeds_for_changes(&tx, R, &[change("other.tx", "modified", &[(1, 1)])]).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn deleted_file_seeds_broken_refs() {
        let mut doc = Id::of("x");
        let (_t, c) = build(|tx| {
            let (d, s) = artifact(tx, R, "doc.md", &[("sec", 1, 30)]);
            doc = s[0];
            let r = OutRef {
                from: s[0],
                kind: LinkKind("links-to".into()),
                span: Some(span(2, 2)),
                candidates: vec![ExportKey {
                    namespace: "path".into(),
                    key: "gone.tx#part".into(),
                }],
                confidence: Confidence::Extracted,
                origin: None,
                binds: None,
            };
            let mut row = katalogr_store::Reader::artifact(tx, d).unwrap().unwrap();
            row.refs = vec![r.clone()];
            tx.put_artifact(d, &row).unwrap();
            tx.put_unresolved(
                RefId {
                    artifact: d,
                    index: 0,
                },
                &r,
            )
            .unwrap();
        });
        let tx = c.read().unwrap();
        let s = seeds_for_changes(&tx, R, &[change("gone.tx", "deleted", &[])]).unwrap();
        assert_eq!(
            s,
            [Seed {
                id: doc,
                precision: SEED_BROKEN_REF,
                wanted: Some("path:gone.tx#part".into())
            }]
        );
        let s = seeds_for_changes(&tx, R, &[change("else.tx", "deleted", &[])]).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn item_seeds_annotated_and_touched() {
        let mut ids = Vec::new();
        let (_t, c) = build(|tx| {
            let (_, item) = artifact(tx, "b", "item.md", &[("FEAT-1", 1, 9), ("req", 2, 3)]);
            let (_, notes) = artifact(tx, R, "notes.md", &[("on_item", 1, 2), ("on_req", 3, 4)]);
            let (_, code) = artifact(tx, R, "code.tx", &[("fn_a", 1, 5)]);
            let (commit, _) = artifact(tx, R, "commit", &[]);
            let code_art = katalogr_model::ArtifactName {
                repo: R.into(),
                path: RelPath::new("code.tx").unwrap(),
            }
            .id();
            tx.put_exports(
                &ExportKey {
                    namespace: ITEM_NS.into(),
                    key: "FEAT-1".into(),
                },
                &[item[0]],
            )
            .unwrap();
            tx.put_exports(
                &ExportKey {
                    namespace: ITEM_NS.into(),
                    key: "FEAT-2".into(),
                },
                &[item[1]],
            )
            .unwrap();
            link(tx, "b", "item.md", item[0], "contains", item[1]);
            link(tx, R, "notes.md", notes[0], "annotated-by", item[0]);
            link(tx, R, "notes.md", notes[1], "annotated-by", item[1]);
            link(tx, R, "commit", commit, "refs-item", item[0]);
            link(tx, R, "commit", commit, "touches", code_art);
            ids = vec![notes[0], notes[1], code_art, code[0]];
        });
        let tx = c.read().unwrap();
        let s = seeds_for_item(&tx, "FEAT-1").unwrap().unwrap();
        assert_eq!(
            shape(&s),
            sorted(ids.iter().map(|i| (*i, SEED_ITEM)).collect())
        );
        // FEAT-2 is a leaf: only what annotates it.
        let s = seeds_for_item(&tx, "FEAT-2").unwrap().unwrap();
        assert_eq!(shape(&s), [(ids[1], SEED_ITEM)]);
        assert!(seeds_for_item(&tx, "FEAT-9").unwrap().is_none());
    }
}
