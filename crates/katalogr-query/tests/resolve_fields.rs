mod support;

mod resolve {
    use super::support::*;
    use katalogr_query::{Resolution, resolve};
    use katalogr_store::Cache;

    fn build(f: impl FnOnce(&mut katalogr_store::WriteTx)) -> (tempfile::TempDir, Cache) {
        let tmp = tempfile::tempdir().unwrap();
        let cache = open(&tmp, &["r"]);
        {
            let mut tx = cache.write().unwrap();
            f(&mut tx);
            tx.commit().unwrap();
        }
        (tmp, cache)
    }

    fn names(r: Resolution<katalogr_query::SymbolHit>) -> Vec<String> {
        match r {
            Resolution::One(h) => vec![h.name],
            Resolution::Ambiguous(v) => v.into_iter().map(|h| h.name).collect(),
            Resolution::None => Vec::new(),
        }
    }

    #[test]
    fn plain_name_prefers_non_field() {
        let (_t, c) = build(|tx| {
            artifact_with(
                tx,
                "r",
                "a.tx",
                vec![
                    sym(&[("fn", "id")], "fn"),
                    sym(&[("type", "Order")], "type"),
                    field(&[("type", "Order"), ("field", "id")], "string", true, false),
                ],
            );
        });
        let tx = c.read().unwrap();
        // The function wins over the field of the same plain name.
        assert_eq!(names(resolve(&tx, "id").unwrap()), ["r:a.tx#fn:id"]);
        // The field is still found by its canonical name.
        assert_eq!(
            names(resolve(&tx, "r:a.tx#type:Order/field:id").unwrap()),
            ["r:a.tx#type:Order/field:id"]
        );
    }

    #[test]
    fn field_found_when_only_match() {
        let (_t, c) = build(|tx| {
            artifact_with(
                tx,
                "r",
                "a.tx",
                vec![
                    sym(&[("type", "Order")], "type"),
                    field(
                        &[("type", "Order"), ("field", "sku")],
                        "string",
                        true,
                        false,
                    ),
                ],
            );
            artifact_with(
                tx,
                "r",
                "b.tx",
                vec![
                    sym(&[("type", "Line")], "type"),
                    field(&[("type", "Line"), ("field", "sku")], "string", true, false),
                ],
            );
        });
        let tx = c.read().unwrap();
        // Only fields match: all stay, so the name is ambiguous between them.
        assert_eq!(
            names(resolve(&tx, "sku").unwrap()),
            ["r:a.tx#type:Order/field:sku", "r:b.tx#type:Line/field:sku"]
        );
        // One field alone resolves.
        let (_t2, c2) = build(|tx| {
            artifact_with(
                tx,
                "r",
                "a.tx",
                vec![
                    sym(&[("type", "Order")], "type"),
                    field(
                        &[("type", "Order"), ("field", "sku")],
                        "string",
                        true,
                        false,
                    ),
                ],
            );
        });
        let tx2 = c2.read().unwrap();
        assert_eq!(
            names(resolve(&tx2, "sku").unwrap()),
            ["r:a.tx#type:Order/field:sku"]
        );
    }
}
