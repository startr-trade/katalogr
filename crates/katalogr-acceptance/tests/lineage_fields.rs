//! FEAT-101: fields of code types in the lineage chain. A Rust struct is a copy of the schema
//! type, bound by a synthetic profile rule; its fields pair with the schema's by name.

mod lineage_chain {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_acceptance::{Case, full_registry, run_case};
    use katalogr_model::Span;
    use katalogr_query::{LineageOptions, LineageReport, SymbolHit, lineage};
    use std::collections::{BTreeMap, BTreeSet};

    fn fixtures() -> Utf8PathBuf {
        Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
    }

    fn hit(name: &str) -> SymbolHit {
        SymbolHit {
            name: name.to_string(),
            kind: if name.starts_with("contract:") {
                "contract"
            } else {
                ""
            }
            .to_string(),
            path: String::new(),
            span: Span {
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 1,
            },
        }
    }

    fn opts() -> LineageOptions {
        let code_kinds: BTreeMap<String, BTreeSet<String>> = full_registry()
            .extractors()
            .map(|e| {
                (
                    e.id().to_string(),
                    e.code_kinds().iter().map(|k| k.to_string()).collect(),
                )
            })
            .filter(|(_, k): &(String, BTreeSet<String>)| !k.is_empty())
            .collect();
        LineageOptions {
            depth: 3,
            limit: 200,
            per_group: 50,
            code_kinds,
            fold_from: Some(50),
        }
    }

    fn open(case: &str) -> Case {
        let dir = fixtures().join("data").join(case);
        let report = run_case(&full_registry, &dir).unwrap();
        assert!(report.passed(), "{}", report.render());
        Case::open(&full_registry, &dir).unwrap()
    }

    fn ask(c: &Case, name: &str) -> LineageReport {
        // Resolve the plain name as the facade does: a canonical name is its own symbol.
        let target = c
            .read(|tx| {
                use katalogr_store::Reader;
                tx.symbol(katalogr_model::Id::of(name)).unwrap()
            })
            .unwrap()
            .map_or_else(
                || hit(name),
                |r| {
                    use katalogr_model::Name;
                    SymbolHit {
                        name: r.name.canonical(),
                        kind: r.kind.clone(),
                        path: r.name.artifact.path.as_str().to_string(),
                        span: r.span,
                    }
                },
            );
        c.read(|tx| lineage(tx, &target, &opts()).unwrap().report)
            .unwrap()
    }

    const ORDER: &str = "fx:schemas/orders.xsd#type:Order";
    const CODE_ORDER: &str = "fx:src/model.rs#struct:Order";

    fn field<'r>(r: &'r LineageReport, name: &str) -> &'r katalogr_query::LineageField {
        r.fields
            .iter()
            .find(|f| f.node.name.ends_with(&format!("field:{name}")))
            .unwrap_or_else(|| panic!("no field {name}"))
    }

    #[test]
    fn code_field_paired_with_schema_field() {
        let c = open("lineage-chain");
        let r = ask(&c, ORDER);
        // Both types declare the contract: the schema's fields come first.
        let declared: Vec<&str> = r.declared_in.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(declared, [ORDER, CODE_ORDER]);
        let pair = |f: &str| -> Vec<(String, String)> {
            field(&r, f)
                .copies
                .iter()
                .map(|c| (c.node.name.clone(), c.by.clone()))
                .collect()
        };
        // Two fields match by exact name, one by normalised name.
        for f in ["id", "total"] {
            assert_eq!(
                pair(f),
                [(format!("{CODE_ORDER}/field:{f}"), "name".to_string())],
                "{f}"
            );
        }
        assert_eq!(
            pair("orderDate"),
            [(
                format!("{CODE_ORDER}/field:order_date"),
                "normalised-name".to_string()
            )]
        );
        // The code fields with no schema field are listed apart.
        let unpaired: Vec<&str> = r.unpaired_fields.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(
            unpaired,
            [
                "fx:src/model.rs#struct:Order/field:internal",
                "fx:src/model.rs#struct:Order/field:lines",
            ]
        );
        // The code type is a copy of the contract, so mirrors compare it as well.
        let drift = c
            .read(|tx| katalogr_query::mirror_drift(tx, &[]).unwrap())
            .unwrap();
        assert!(
            drift
                .iter()
                .any(|d| d.comparable && d.right.name == CODE_ORDER),
            "{drift:?}"
        );
    }

    #[test]
    fn code_type_related_through_fields() {
        let c = open("lineage-chain");
        let r = ask(&c, CODE_ORDER);
        assert_eq!(
            r.subject_contracts,
            ["contract:schema_type:{urn%3Aexample%3Aorders}Order"]
        );
        let uses: Vec<(&str, &str, Option<&str>)> = r
            .related
            .iter()
            .map(|x| {
                (
                    x.relation.as_str(),
                    x.node.name.as_str(),
                    x.through_field.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            uses,
            [("uses", "fx:src/model.rs#struct:Line", Some("lines"))]
        );
        // The other way: the line type is used by the order, through its field.
        let l = ask(&c, "fx:src/model.rs#struct:Line");
        let used_by: Vec<(&str, &str, Option<&str>)> = l
            .related
            .iter()
            .map(|x| {
                (
                    x.relation.as_str(),
                    x.node.name.as_str(),
                    x.through_field.as_deref(),
                )
            })
            .collect();
        assert_eq!(used_by, [("used-by", CODE_ORDER, Some("lines"))]);
    }
}
