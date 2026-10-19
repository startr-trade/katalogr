//! Bindings in fragments (FEAT-002 R-1).

mod contract {
    use katalogr_model::{
        ArtifactName, ArtifactNode, Binding, Confidence, ContractName, ContractRole, Fragment, Id,
        Name, Provenance, RelPath, Span,
    };
    use std::collections::BTreeMap;

    fn fragment(bindings: Vec<Binding>) -> Fragment {
        let path = RelPath::new("a.tx").unwrap();
        Fragment {
            provenance: Provenance {
                repo: "r".into(),
                path: path.clone(),
                revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
            },
            extension: "test".into(),
            artifact: ArtifactNode {
                name: ArtifactName {
                    repo: "r".into(),
                    path,
                },
                facts: BTreeMap::new(),
            },
            symbols: Vec::new(),
            links: Vec::new(),
            refs: Vec::new(),
            exports: Vec::new(),
            bindings,
        }
    }

    fn binding(role: ContractRole) -> Binding {
        Binding {
            symbol: Id::of("r:a.tx#fn:f"),
            contract: ContractName {
                kind: "channel".into(),
                key: "orders/new".into(),
            },
            role,
            span: Some(Span {
                start_line: 2,
                start_col: 1,
                end_line: 2,
                end_col: 9,
            }),
            confidence: Confidence::Declared,
            origin: None,
        }
    }

    #[test]
    fn binding_recorded() {
        // A binding survives a JSON round trip, with all three roles.
        for role in [
            ContractRole::Provides,
            ContractRole::Consumes,
            ContractRole::Declares,
        ] {
            let f = fragment(vec![binding(role)]);
            let json = serde_json::to_string(&f).unwrap();
            let back: Fragment = serde_json::from_str(&json).unwrap();
            assert_eq!(back, f);
            assert_eq!(back.bindings[0].role, role);
            assert_eq!(
                back.bindings[0].contract.canonical(),
                "contract:channel:orders%2Fnew"
            );
        }
        // Role names are the link kinds the stitcher writes.
        assert_eq!(ContractRole::Provides.as_str(), "provides");
        assert_eq!(ContractRole::Consumes.as_str(), "consumes");
        assert_eq!(ContractRole::Declares.as_str(), "declares");

        // A fragment without bindings serialises without the field, and a fragment written
        // before bindings existed still reads (shard format 1).
        let empty = fragment(Vec::new());
        let mut value = serde_json::to_value(&empty).unwrap();
        assert!(value.get("bindings").is_none());
        assert_eq!(
            serde_json::from_value::<Fragment>(value.clone()).unwrap(),
            empty
        );
        value.as_object_mut().unwrap().remove("bindings");
        assert_eq!(serde_json::from_value::<Fragment>(value).unwrap(), empty);
    }
}
