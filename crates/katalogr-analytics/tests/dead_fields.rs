//! A symbol with the neutral `field_type` fact is never a dead-code candidate (FEAT-101 R-20).

mod dead {
    use camino::Utf8PathBuf;
    use katalogr_analytics::options::STRUCTURAL_KINDS;
    use katalogr_analytics::{DeadCodeOptions, DeadConfidence, dead_code};
    use katalogr_model::{
        ArtifactName, Confidence, Fact, LinkKind, Name, Provenance, RelPath, Segment, Span,
        SymbolName,
    };
    use katalogr_store::{
        ArtifactRow, Cache, Kindex, LinkEntry, LinkKey, LinkOrigin, LinkRow, ProjectConfig,
        ProjectName, RepoConfig, SymbolRow,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::{Duration, Instant};

    const SPAN: Span = Span {
        start_line: 1,
        start_col: 1,
        end_line: 1,
        end_col: 1,
    };

    #[test]
    fn field_symbols_never_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("dead").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let cache = Cache::open(&dir, Instant::now() + Duration::from_secs(30)).unwrap();

        let rel = RelPath::new("a.tx").unwrap();
        let aname = ArtifactName {
            repo: "r".into(),
            path: rel.clone(),
        };
        let aid = aname.id();
        let prov = Provenance {
            repo: "r".into(),
            path: rel.clone(),
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        };
        let name = |segs: &[(&str, &str)]| SymbolName {
            artifact: aname.clone(),
            segments: segs
                .iter()
                .map(|(k, n)| Segment {
                    kind: (*k).into(),
                    name: (*n).into(),
                })
                .collect(),
            ordinal: 1,
        };
        let row = |n: SymbolName, kind: &str, facts: &[(&str, Fact)]| SymbolRow {
            name: n,
            artifact: aid,
            kind: kind.into(),
            span: SPAN,
            facts: facts
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        };
        let main = name(&[("fn", "main")]);
        let owner = name(&[("struct", "Unused")]);
        let fld = name(&[("struct", "Unused"), ("field", "id")]);
        let (main_id, owner_id, fld_id) = (main.id(), owner.id(), fld.id());
        {
            let mut tx = cache.write().unwrap();
            tx.put_symbol(
                main_id,
                &row(main, "fn", &[("root", Fact::Str("main".into()))]),
            )
            .unwrap();
            tx.put_symbol(owner_id, &row(owner, "struct", &[])).unwrap();
            tx.put_symbol(
                fld_id,
                &row(
                    fld,
                    "field",
                    &[
                        ("field_type", Fact::Str("string".into())),
                        ("required", Fact::Bool(true)),
                        ("many", Fact::Bool(false)),
                    ],
                ),
            )
            .unwrap();
            tx.put_link(
                &LinkKey {
                    from: owner_id,
                    kind: LinkKind("contains".into()),
                    to: fld_id,
                },
                &LinkRow {
                    entries: vec![LinkEntry {
                        origin: LinkOrigin::Intra,
                        confidence: Confidence::Extracted,
                        span: Some(SPAN),
                        provenance: prov.clone(),
                        fanout: None,
                        produced_by: None,
                    }],
                },
            )
            .unwrap();
            let mut syms = vec![main_id, owner_id, fld_id];
            syms.sort();
            tx.put_artifact(
                aid,
                &ArtifactRow {
                    name: aname.clone(),
                    extension: "tx".into(),
                    provenance: prov,
                    facts: BTreeMap::new(),
                    symbols: syms,
                    exports: Vec::new(),
                    refs: Vec::new(),
                },
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let opts = DeadCodeOptions {
            code_kinds: BTreeMap::from([(
                "tx".to_string(),
                BTreeSet::from(["fn".to_string(), "struct".to_string()]),
            )]),
            exclude_kinds: STRUCTURAL_KINDS.iter().map(|k| k.to_string()).collect(),
            internal: false,
            min_confidence: DeadConfidence::Low,
            path_prefix: None,
            limit: 100,
        };
        let report = dead_code(&cache.read().unwrap(), &opts).unwrap();
        assert!(report.note.is_none(), "{:?}", report.note);
        let names: Vec<&str> = report
            .items
            .iter()
            .map(|i| i.symbol.name.as_str())
            .collect();
        assert!(
            names.iter().any(|n| n.ends_with("#struct:Unused")),
            "the type itself is a candidate: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("/field:")),
            "a field is never a candidate: {names:?}"
        );
    }
}
