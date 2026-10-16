mod graph {
    use katalogr_model::*;

    fn syms(repo: &str, path: &str, names: &[&str]) -> Vec<SymbolName> {
        let a = ArtifactName::new(repo, path).unwrap();
        names
            .iter()
            .map(|n| SymbolName {
                artifact: a.clone(),
                segments: vec![Segment {
                    kind: "fn".into(),
                    name: (*n).into(),
                }],
                ordinal: 1,
            })
            .collect()
    }

    #[test]
    fn stable_names_unaffected_by_unrelated_edits() {
        let a_before = syms("core", "src/a.txt", &["one", "two"]);
        let b_before = syms("core", "src/b.txt", &["x"]);
        // Edit only B: different symbols, content-derived inputs.
        let a_after = syms("core", "src/a.txt", &["one", "two"]);
        let b_after = syms("core", "src/b.txt", &["x", "y", "z"]);
        assert_ne!(b_before.len(), b_after.len());
        let canon = |v: &[SymbolName]| {
            v.iter()
                .map(|n| (n.canonical(), n.id()))
                .collect::<Vec<_>>()
        };
        assert_eq!(canon(&a_before), canon(&a_after));
    }

    #[test]
    fn link_carries_kind_direction_confidence_provenance() {
        let from = Id::of("core:a.txt#fn:f");
        let to = Id::of("core:b.txt#fn:g");
        let link = Link {
            from,
            to,
            kind: LinkKind("calls".into()),
            confidence: Confidence::Inferred,
            span: Some(Span {
                start_line: 3,
                start_col: 1,
                end_line: 3,
                end_col: 9,
            }),
            provenance: Provenance {
                repo: "core".into(),
                path: RelPath::new("a.txt").unwrap(),
                revision: "abc123".into(),
            },
        };
        let json = serde_json::to_string(&link).unwrap();
        let back: Link = serde_json::from_str(&json).unwrap();
        assert_eq!(back, link);
        assert_eq!(back.kind.0, "calls");
        assert_eq!(back.confidence, Confidence::Inferred);
        assert_eq!(back.span.unwrap().end_col, 9);
        assert_eq!(back.provenance.revision, "abc123");
        assert_eq!(back.provenance.path.as_str(), "a.txt");
        assert_ne!(back.from, back.to);
        assert_eq!((back.from, back.to), (from, to));
        assert!(json.contains(&from.to_hex()));
    }
}
