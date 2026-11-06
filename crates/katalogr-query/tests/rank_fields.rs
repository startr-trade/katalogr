mod support;

mod rank {
    use super::support::*;
    use katalogr_query::rank_live;
    use std::collections::BTreeMap;

    /// The ranks by name of a small graph, with or without a field of the type.
    fn ranks(with_field: bool) -> BTreeMap<String, f64> {
        let tmp = tempfile::tempdir().unwrap();
        let cache = open(&tmp, &["r"]);
        {
            let mut tx = cache.write().unwrap();
            let mut syms = vec![
                sym(&[("type", "Holder")], "type"),
                sym(&[("fn", "user")], "fn"),
            ];
            if with_field {
                syms.push(field(
                    &[("type", "Holder"), ("field", "x")],
                    "string",
                    true,
                    false,
                ));
            }
            let a = artifact_with(&mut tx, "r", "a.tx", syms);
            link(&mut tx, "r", "a.tx", a[1], "calls", a[0]);
            if with_field {
                link(&mut tx, "r", "a.tx", a[0], "contains", a[2]);
                link(&mut tx, "r", "a.tx", a[1], "calls", a[2]);
            }
            tx.commit().unwrap();
        }
        let tx = cache.read().unwrap();
        rank_live(&tx)
            .unwrap()
            .into_iter()
            .map(|(h, r)| (h.name, r))
            .collect()
    }

    #[test]
    fn fields_take_no_rank() {
        let without = ranks(false);
        let with = ranks(true);
        // The field has no rank of its own ...
        assert_eq!(with["r:a.tx#type:Holder/field:x"], 0.0);
        // ... and the code symbols rank as they did before the field existed, bit for bit.
        for (name, rank) in &without {
            assert_eq!(with[name].to_bits(), rank.to_bits(), "{name}");
        }
    }
}
