//! FEAT-101: type lineage across extensions. A schema type, an API component type and a store
//! definition reach each other, an operation, code and a table through contracts. The profile
//! is a synthetic example; names are invented.

mod lineage_chain {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_acceptance::{Case, full_registry, run_case};
    use katalogr_model::{Confidence, Span};
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

    fn names(groups: &[katalogr_query::LineageGroup]) -> Vec<(String, String)> {
        groups
            .iter()
            .flat_map(|g| {
                g.hits
                    .iter()
                    .map(|h| (g.kind.clone(), h.node.name.clone()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    const ORDER: &str = "fx:schemas/orders.xsd#type:Order";

    #[test]
    fn type_reaches_operation_and_code() {
        let c = open("lineage-chain");
        let r = ask(&c, "fx:api/openapi.yaml#type:Receipt");
        let bindings = names(&r.bindings);
        // The operation that takes the type.
        assert!(
            bindings.contains(&(
                "uses-type".into(),
                "fx:api/openapi.yaml#operation:POST%20%2Forders".into()
            )),
            "{bindings:?}"
        );
        // Through the operation's route: the handler and the client, as code.
        let code = names(&r.code);
        assert!(
            code.contains(&("provides".into(), "fx:src/server.rs#fn:place".into())),
            "{code:?}"
        );
        assert!(
            code.contains(&("consumes".into(), "fx:src/client.rs#fn:send".into())),
            "{code:?}"
        );
        assert!(
            r.contracts
                .iter()
                .any(|c| c.contract == "contract:http_route:POST%20%2Forders")
        );
        assert!(!r.boundaries.contains(&"no-code".to_string()));
    }

    #[test]
    fn type_reaches_definition_and_contract_user() {
        let c = open("codec-chain");
        let r = ask(&c, "fx:schemas/orders.xsd#type:Order");
        let all: Vec<(String, String)> = names(&r.bindings)
            .into_iter()
            .chain(names(&r.code))
            .collect();
        // The definition file's key uses the type, and its document provides the codec.
        let key = "fx:codecs/orders.yaml#doc:orders/key:schema/key:type";
        assert!(all.contains(&("uses-type".into(), key.into())), "{all:?}");
        let hit = r
            .bindings
            .iter()
            .flat_map(|g| &g.hits)
            .find(|h| h.node.name == key)
            .unwrap();
        assert_eq!(
            hit.holder.as_deref(),
            Some("fx:codecs/orders.yaml#doc:orders")
        );
        // One hop through the codec contract: the channel that consumes it.
        let channel = (
            "consumes".to_string(),
            "fx:channels/orders.tx#channel:orders".to_string(),
        );
        assert!(all.contains(&channel), "{all:?}");
        assert!(
            r.contracts
                .iter()
                .any(|c| c.contract == "contract:codec:urn%3Aexample%3Aorders")
        );
    }

    #[test]
    fn profile_binds_store_and_table() {
        let c = open("lineage-chain");
        let r = ask(&c, ORDER);
        let bindings = names(&r.bindings);
        // The store definition uses the type; its document consumes the table.
        assert!(
            bindings.contains(&(
                "uses-type".into(),
                "fx:stores/orders.yaml#doc:orders/key:type".into()
            )),
            "{bindings:?}"
        );
        // The table's own definition and the code that queries it are on the other side.
        assert!(
            bindings.contains(&(
                "provides".into(),
                "fx:db/orders.sql#table:public.orders".into()
            )),
            "{bindings:?}"
        );
        let code = names(&r.code);
        assert!(
            code.contains(&("consumes".into(), "fx:src/repo.rs#fn:load_orders".into())),
            "{code:?}"
        );
        assert!(
            r.contracts
                .iter()
                .any(|c| c.contract == "contract:sql_table:public.orders")
        );
    }

    #[test]
    fn field_lists_its_mappings() {
        let c = open("lineage-chain");
        let r = ask(&c, ORDER);
        let id = r
            .fields
            .iter()
            .find(|f| f.node.name.ends_with("field:id"))
            .expect("field id");
        assert_eq!(id.mapped_by.len(), 1, "{:?}", id.mapped_by);
        let m = &id.mapped_by[0];
        assert_eq!(
            m.node.name,
            "fx:stores/orders.yaml#doc:orders/key:columns/key:order_id"
        );
        assert_eq!(
            (m.kind.as_str(), m.confidence),
            ("maps-to", Confidence::Declared)
        );
        assert_eq!(
            m.facts.get("wire_format").map(String::as_str),
            Some("column")
        );
        assert_eq!(
            m.facts.get("wire_field").map(String::as_str),
            Some("order_id")
        );
    }

    fn copy_without_profiles(from: &Utf8Path, to: &Utf8Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().into_string().unwrap();
            if name == ".katalogr-profiles" || name == "expected.yaml" {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                copy_without_profiles(&from.join(&name), &to.join(&name));
            } else {
                std::fs::copy(from.join(&name), to.join(&name)).unwrap();
            }
        }
    }

    #[test]
    fn no_profile_says_where_it_stops() {
        let tmp = tempfile::tempdir().unwrap();
        let to = Utf8Path::from_path(tmp.path()).unwrap().join("bare");
        copy_without_profiles(&fixtures().join("data/lineage-chain"), &to);
        let c = Case::open(&full_registry, &to).unwrap();
        let r = ask(&c, ORDER);
        // Without the profile nothing binds the type to the store or the table.
        let all: Vec<_> = names(&r.bindings)
            .into_iter()
            .chain(names(&r.code))
            .collect();
        assert!(all.is_empty(), "{all:?}");
        assert!(
            r.boundaries.contains(&"no-code".to_string()),
            "{:?}",
            r.boundaries
        );
        assert!(r.boundaries.contains(&"open-contract".to_string()));
        assert!(!r.boundaries.contains(&"no-contract".to_string()));
    }
}
