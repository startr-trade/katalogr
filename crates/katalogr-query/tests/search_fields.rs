mod support;

mod search {
    use super::support::*;
    use katalogr_model::{Field, Id, tokenize};
    use katalogr_query::search;
    use katalogr_store::{LEXICON_FIELDS, WriteTx};
    use std::collections::BTreeMap;

    /// Index a symbol whose only searchable text is its name.
    fn index_name(tx: &mut WriteTx, id: Id, name: &str) {
        let mut lens = [0u32; LEXICON_FIELDS];
        let mut terms: BTreeMap<(String, u8), u32> = BTreeMap::new();
        for t in tokenize(name) {
            lens[Field::Name.index()] += 1;
            *terms.entry((t, Field::Name.index() as u8)).or_default() += 1;
        }
        let terms: Vec<(String, u8, u32)> =
            terms.into_iter().map(|((t, f), n)| (t, f, n)).collect();
        tx.put_doc_postings(id, lens, &terms).unwrap();
    }

    #[test]
    fn field_ranks_below_owner_and_peers() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = open(&tmp, &["r"]);
        {
            let mut tx = cache.write().unwrap();
            // The field's canonical name sorts first, so a tie would put it first.
            let a = artifact_with(
                &mut tx,
                "r",
                "a.tx",
                vec![
                    sym(&[("type", "Holder")], "type"),
                    field(
                        &[("type", "Holder"), ("field", "widget")],
                        "string",
                        true,
                        false,
                    ),
                ],
            );
            link(&mut tx, "r", "a.tx", a[0], "contains", a[1]);
            let b = artifact_with(&mut tx, "r", "b.tx", vec![sym(&[("fn", "widget")], "fn")]);
            let c = artifact_with(
                &mut tx,
                "r",
                "c.tx",
                vec![sym(&[("type", "widget")], "type")],
            );
            for id in [a[1], b[0], c[0]] {
                index_name(&mut tx, id, "widget");
            }
            tx.commit().unwrap();
        }
        let tx = cache.read().unwrap();
        let hits = search(&tx, "widget", 10).unwrap();
        let names: Vec<&str> = hits.iter().map(|h| h.symbol.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "r:b.tx#fn:widget",
                "r:c.tx#type:widget",
                "r:a.tx#type:Holder/field:widget"
            ]
        );
        // The printed score is still the match score: equal for all three.
        assert_eq!(hits[0].score, hits[2].score);
    }
}
