mod support;

mod lineage {
    use super::support::*;
    use katalogr_model::{Confidence, Id};
    use katalogr_query::{LineageOptions, LineageReport, SymbolHit, lineage};
    use katalogr_store::{Cache, WriteTx};

    const R: &str = "r";

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

    fn run(cache: &Cache, name: &str) -> LineageReport {
        use katalogr_store::Reader;
        let tx = cache.read().unwrap();
        let row = tx.symbol(Id::of(name)).unwrap().expect(name);
        let t = SymbolHit {
            name: name.to_string(),
            kind: row.kind.clone(),
            path: row.name.artifact.path.as_str().to_string(),
            span: row.span,
        };
        let o = LineageOptions {
            depth: 3,
            limit: 200,
            per_group: 10,
            code_kinds: Default::default(),
            fold_from: Some(50),
        };
        lineage(&tx, &t, &o).unwrap().report
    }

    /// A type with the given field names in a file, declaring the shape contract.
    fn typ(tx: &mut WriteTx, path: &str, declares: bool, names: &[&'static str]) {
        let mut syms = vec![sym(&[("type", "Order")], "type")];
        for n in names {
            syms.push(field(
                &[("type", "Order"), ("field", n)],
                "string",
                true,
                false,
            ));
        }
        let ids = artifact_with(tx, R, path, syms);
        for f in &ids[1..] {
            link(tx, R, path, ids[0], "contains", *f);
        }
        if declares {
            let c = contract(tx, "shape", "Order", 0, 0, 3);
            link(tx, R, path, ids[0], "declares", c);
        }
    }

    fn copies(r: &LineageReport, field: &str) -> Vec<(String, String)> {
        r.fields
            .iter()
            .find(|f| f.node.name.ends_with(&format!("field:{field}")))
            .unwrap()
            .copies
            .iter()
            .map(|c| (c.node.name.clone(), c.by.clone()))
            .collect()
    }

    const A: &str = "r:a.tx#type:Order";

    #[test]
    fn copies_pair_fields_by_exact_name() {
        let (_t, c) = build(|tx| {
            typ(tx, "a.tx", true, &["id", "total"]);
            typ(tx, "b.tx", true, &["id", "extra"]);
        });
        let r = run(&c, A);
        assert_eq!(
            copies(&r, "id"),
            [("r:b.tx#type:Order/field:id".to_string(), "name".to_string())]
        );
        assert!(copies(&r, "total").is_empty());
        let un: Vec<_> = r.unpaired_fields.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(un, ["r:b.tx#type:Order/field:extra"]);
    }

    #[test]
    fn normalised_name_pairs_only_when_unique() {
        let (_t, c) = build(|tx| {
            typ(tx, "a.tx", true, &["order_id", "x_y", "xy"]);
            typ(tx, "b.tx", true, &["orderId", "xY"]);
        });
        let r = run(&c, A);
        assert_eq!(
            copies(&r, "order_id"),
            [(
                "r:b.tx#type:Order/field:orderId".to_string(),
                "normalised-name".to_string()
            )]
        );
        // `x_y`, `xy` and `xY` all normalise to `xy`: not unique, so nothing pairs.
        assert!(copies(&r, "x_y").is_empty());
        assert!(copies(&r, "xy").is_empty());
        assert_eq!(r.unpaired_fields.len(), 1);
    }

    #[test]
    fn no_pairing_outside_a_copy_set() {
        // Alike fields, but the second type declares no contract of the first's: no copies.
        let (_t, c) = build(|tx| {
            typ(tx, "a.tx", true, &["id"]);
            typ(tx, "b.tx", false, &["id"]);
        });
        let r = run(&c, A);
        assert!(copies(&r, "id").is_empty());
        assert!(r.unpaired_fields.is_empty());
        assert_eq!(r.declared_in.len(), 1);
    }

    #[test]
    fn maps_to_kept_with_its_confidence() {
        let (_t, c) = build(|tx| {
            typ(tx, "a.tx", true, &["id"]);
            typ(tx, "b.tx", true, &["id"]);
            let m = artifact_with(tx, R, "m.tx", vec![sym(&[("key", "k")], "key")]);
            link_conf(
                tx,
                R,
                "m.tx",
                m[0],
                "maps-x",
                Id::of("r:a.tx#type:Order/field:id"),
                Confidence::Declared,
            );
        });
        let r = run(&c, A);
        let f = &r.fields[0];
        // The recorded mapping keeps its own confidence; the pairing is a separate, inferred list.
        assert_eq!(f.mapped_by.len(), 1);
        assert_eq!(f.mapped_by[0].confidence, Confidence::Declared);
        assert_eq!(f.copies.len(), 1);
    }
}
