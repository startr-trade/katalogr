mod support;

mod doc_impact {
    use super::support::*;
    use katalogr_model::{Confidence, ExportKey, Id, LinkKind, OutRef, RelPath};
    use katalogr_query::{ChangedFile, DOC_LINK_KINDS, DocImpact, DocImpactInput, doc_impact};
    use katalogr_store::{Cache, FanKey, RefId};

    const R: &str = "r";

    fn change(path: &str, kind: &str, ranges: &[(u32, u32)]) -> ChangedFile {
        ChangedFile {
            path: RelPath::new(path).unwrap(),
            kind: kind.into(),
            ranges: ranges.to_vec(),
        }
    }

    fn run(cache: &Cache, changes: &[ChangedFile], kinds: &[&str], limit: usize) -> DocImpact {
        let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
        let tx = cache.read().unwrap();
        doc_impact(
            &tx,
            &DocImpactInput {
                repo: R,
                changes,
                kinds: &kinds,
                limit,
            },
        )
        .unwrap()
    }

    #[derive(Clone, Copy)]
    struct Ids {
        code: Id,
        fn_a: Id,
        fn_b: Id,
        doc: Id,
        sec: Id,
    }

    /// `code.tx` with `fn_a` (lines 1-5) and `fn_b` (10-20); `doc.md` with section `sec`.
    struct Fx {
        _tmp: tempfile::TempDir,
        cache: Cache,
    }

    fn fx(build: impl FnOnce(&mut katalogr_store::WriteTx, &Ids)) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let cache = open(&tmp, &[R]);
        {
            let mut tx = cache.write().unwrap();
            let (code, s) = artifact(&mut tx, R, "code.tx", &[("fn_a", 1, 5), ("fn_b", 10, 20)]);
            let (doc, d) = artifact(&mut tx, R, "doc.md", &[("sec", 1, 30)]);
            let ids = Ids {
                code,
                fn_a: s[0],
                fn_b: s[1],
                doc,
                sec: d[0],
            };
            build(&mut tx, &ids);
            tx.commit().unwrap();
        }
        Fx { _tmp: tmp, cache }
    }

    #[test]
    fn symbol_link_needs_changed_line() {
        let f = fx(|tx, f| link(tx, R, "doc.md", f.sec, "links-to", f.fn_a));
        let hit = run(
            &f.cache,
            &[change("code.tx", "modified", &[(2, 2)])],
            &[],
            0,
        );
        assert_eq!(hit.docs.len(), 1);
        let l = &hit.docs[0].links[0];
        assert_eq!(
            (l.precision.as_str(), l.change.as_str()),
            ("symbol", "modified")
        );
        assert_eq!(l.kind, "links-to");
        assert_eq!(hit.docs[0].doc.path, "doc.md");
        assert_eq!(hit.changed_files, 1);
        // a changed line between the symbols touches neither
        let miss = run(
            &f.cache,
            &[change("code.tx", "modified", &[(7, 8)])],
            &[],
            0,
        );
        assert!(miss.docs.is_empty());
    }

    #[test]
    fn file_link_on_any_change() {
        let f = fx(|tx, f| link(tx, R, "doc.md", f.sec, "links-to", f.code));
        for ranges in [&[(7, 8)][..], &[]] {
            let r = run(&f.cache, &[change("code.tx", "modified", ranges)], &[], 0);
            assert_eq!(r.docs.len(), 1);
            assert_eq!(r.docs[0].links[0].precision, "file");
        }
        let added = run(&f.cache, &[change("code.tx", "added", &[])], &[], 0);
        assert_eq!(added.docs[0].links[0].change, "added");
    }

    #[test]
    fn unrelated_symbol_not_reported() {
        let f = fx(|tx, f| link(tx, R, "doc.md", f.sec, "links-to", f.fn_b));
        let r = run(
            &f.cache,
            &[change("code.tx", "modified", &[(1, 3)])],
            &[],
            0,
        );
        assert!(r.docs.is_empty());
        let r = run(
            &f.cache,
            &[change("other.tx", "modified", &[(1, 3)])],
            &[],
            0,
        );
        assert!(r.docs.is_empty());
    }

    #[test]
    fn deleted_target_reported() {
        let f = fx(|tx, f| {
            let r = OutRef {
                from: f.sec,
                kind: LinkKind("links-to".into()),
                span: Some(span(2, 2)),
                candidates: vec![ExportKey {
                    namespace: "path".into(),
                    key: "gone.tx".into(),
                }],
                confidence: Confidence::Extracted,
                origin: None,
                binds: None,
            };
            tx.put_unresolved(
                RefId {
                    artifact: f.doc,
                    index: 0,
                },
                &r,
            )
            .unwrap();
        });
        let r = run(&f.cache, &[change("gone.tx", "deleted", &[])], &[], 0);
        assert_eq!(r.docs.len(), 1);
        let l = &r.docs[0].links[0];
        assert_eq!(l.change, "deleted");
        assert_eq!(l.target, "path:gone.tx");
        // another deleted file does not match
        let r = run(&f.cache, &[change("else.tx", "deleted", &[])], &[], 0);
        assert!(r.docs.is_empty());
    }

    #[test]
    fn sorted_and_deterministic() {
        let f = fx(|tx, f| {
            link(tx, R, "doc.md", f.sec, "links-to", f.fn_b);
            link(tx, R, "doc.md", f.sec, "links-to", f.fn_a);
            link(tx, R, "doc.md", f.sec, "specifies", f.code);
            for p in ["z.md", "a.md", "m.md"] {
                let (_, s) = artifact(tx, R, p, &[("s", 1, 2)]);
                link(tx, R, p, s[0], "links-to", f.fn_a);
            }
        });
        let ch = [change("code.tx", "modified", &[(1, 30)])];
        let a = run(&f.cache, &ch, &[], 0);
        let b = run(&f.cache, &ch, &[], 0);
        assert_eq!(a, b);
        let names: Vec<_> = a.docs.iter().map(|d| d.doc.path.clone()).collect();
        assert_eq!(names, ["a.md", "doc.md", "m.md", "z.md"]);
        let links = &a.docs[1].links;
        let mut sorted = links.clone();
        sorted.sort_by(|x, y| (&x.target, &x.kind).cmp(&(&y.target, &y.kind)));
        assert_eq!(links, &sorted);
        assert_eq!(links.len(), 3);
    }

    #[test]
    fn capped_is_partial() {
        let f = fx(|tx, f| {
            for p in ["a.md", "b.md", "c.md"] {
                let (_, s) = artifact(tx, R, p, &[("s", 1, 2)]);
                link(tx, R, p, s[0], "links-to", f.fn_a);
            }
        });
        let ch = [change("code.tx", "modified", &[(1, 2)])];
        let r = run(&f.cache, &ch, &[], 2);
        assert_eq!(r.docs.len(), 2);
        assert_eq!(r.docs[0].doc.path, "a.md");
        let p = &r.partial[0];
        assert_eq!(
            (p.cap.as_str(), p.at, p.shown, p.total),
            ("limit", 2, 2, Some(3))
        );
        assert!(run(&f.cache, &ch, &[], 3).partial.is_empty());
    }

    #[test]
    fn doc_changed_flag() {
        let f = fx(|tx, f| {
            link(tx, R, "doc.md", f.sec, "links-to", f.fn_a);
            let (_, s) = artifact(tx, R, "other.md", &[("s", 1, 2)]);
            link(tx, R, "other.md", s[0], "links-to", f.fn_a);
        });
        let ch = [
            change("code.tx", "modified", &[(1, 2)]),
            change("doc.md", "modified", &[(3, 3)]),
        ];
        let r = run(&f.cache, &ch, &[], 0);
        let flag = |p: &str| r.docs.iter().find(|d| d.doc.path == p).unwrap().doc_changed;
        assert!(flag("doc.md"));
        assert!(!flag("other.md"));
    }

    #[test]
    fn fan_out_targets_included() {
        let f = fx(|tx, f| {
            let key = ExportKey {
                namespace: "m".into(),
                key: "area".into(),
            };
            let mut targets = vec![f.fn_a, f.fn_b];
            targets.sort();
            tx.put_exports(&key, &targets).unwrap();
            tx.put_fan(
                &FanKey {
                    from: f.sec,
                    kind: LinkKind("links-to".into()),
                    key,
                },
                &link_row(R, "doc.md"),
            )
            .unwrap();
        });
        let r = run(
            &f.cache,
            &[change("code.tx", "modified", &[(12, 12)])],
            &[],
            0,
        );
        assert_eq!(r.docs.len(), 1);
        assert!(r.docs[0].links[0].target.contains("fn_b"));
    }

    #[test]
    fn custom_kind_setting() {
        let f = fx(|tx, f| link(tx, R, "doc.md", f.sec, "custom-kind", f.fn_a));
        let ch = [change("code.tx", "modified", &[(1, 2)])];
        assert!(run(&f.cache, &ch, &[], 0).docs.is_empty());
        assert!(!DOC_LINK_KINDS.contains(&"custom-kind"));
        let r = run(&f.cache, &ch, &["custom-kind"], 0);
        assert_eq!(r.docs[0].links[0].kind, "custom-kind");
        // the default kinds are not read when a setting replaces them
        let f2 = fx(|tx, f| link(tx, R, "doc.md", f.sec, "links-to", f.fn_a));
        assert!(run(&f2.cache, &ch, &["custom-kind"], 0).docs.is_empty());
    }

    #[test]
    fn outgoing_documented_by_found() {
        let f = fx(|tx, f| link(tx, R, "code.tx", f.fn_a, "documented-by", f.sec));
        let r = run(
            &f.cache,
            &[change("code.tx", "modified", &[(1, 2)])],
            &[],
            0,
        );
        assert_eq!(r.docs[0].doc.path, "doc.md");
        assert_eq!(r.docs[0].links[0].kind, "documented-by");
    }
}
