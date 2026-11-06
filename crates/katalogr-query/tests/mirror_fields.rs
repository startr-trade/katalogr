mod support;

mod mirror {
    use super::support::*;
    use katalogr_query::mirror_drift;

    /// Two copies of one contract, the left with `left_ty` and the right with `right_ty` on a
    /// field `line`. Returns the number of drifts and the differences of the first.
    fn drift(left_ty: &'static str, right_ty: &'static str) -> (usize, Vec<String>, bool) {
        let tmp = tempfile::tempdir().unwrap();
        let cache = open(&tmp, &["r"]);
        {
            let mut tx = cache.write().unwrap();
            let l = artifact_with(
                &mut tx,
                "r",
                "a.tx",
                vec![
                    sym(&[("type", "Order")], "type"),
                    field(
                        &[("type", "Order"), ("field", "line")],
                        left_ty,
                        true,
                        false,
                    ),
                ],
            );
            let r = artifact_with(
                &mut tx,
                "r",
                "b.tx",
                vec![
                    sym(&[("struct", "Order")], "struct"),
                    field(
                        &[("struct", "Order"), ("field", "line")],
                        right_ty,
                        true,
                        false,
                    ),
                ],
            );
            link(&mut tx, "r", "a.tx", l[0], "contains", l[1]);
            link(&mut tx, "r", "b.tx", r[0], "contains", r[1]);
            let c = contract(&mut tx, "shape", "Order", 0, 0, 2);
            link(&mut tx, "r", "a.tx", l[0], "declares", c);
            link(&mut tx, "r", "b.tx", r[0], "declares", c);
            tx.commit().unwrap();
        }
        let tx = cache.read().unwrap();
        let d = mirror_drift(&tx, &[]).unwrap();
        let comparable = d.first().is_none_or(|x| x.comparable);
        (
            d.len(),
            d.first()
                .map(|x| x.differences.iter().map(|f| f.what.clone()).collect())
                .unwrap_or_default(),
            comparable,
        )
    }

    #[test]
    fn named_types_compare_by_local_name() {
        // A qualified name, a namespaced key and a dotted path of one type agree.
        for (a, b) in [
            ("{urn:x}Line", "crate::model::Line"),
            ("a/b/Line", "pkg.Line"),
            ("Line", "{urn:x}Line"),
        ] {
            assert_eq!(drift(a, b).0, 0, "{a} vs {b}");
        }
        // Different local names still differ.
        let (n, what, _) = drift("{urn:x}Line", "crate::model::Item");
        assert_eq!((n, what), (1, vec!["type".to_string()]));
        // Primitives compare as they are, never by a trailing piece.
        assert_eq!(drift("string", "integer").0, 1);
        assert_eq!(drift("string", "crate::string").0, 1);
    }

    #[test]
    fn code_copy_now_comparable() {
        // A copy with fields next to a schema copy: comparable, and no drift when they agree.
        let (n, _, comparable) = drift("{urn:x}Line", "crate::model::Line");
        assert!(comparable);
        assert_eq!(n, 0);
    }
}
