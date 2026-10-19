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

    fn span(l: u32) -> Span {
        Span {
            start_line: l,
            start_col: 1,
            end_line: l,
            end_col: 9,
        }
    }

    #[test]
    fn str_calls_fact_capped_and_quoted() {
        use katalogr_model::calls::*;
        assert_eq!(quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert!(token_like("/users/{id}"));
        assert!(!token_like("two words"));
        assert!(!token_like(""));
        assert!(!token_like(&"x".repeat(STR_VALUE_MAX + 1)));
        assert_eq!(
            redact("https://u:p@h.example.test/x"),
            "https://h.example.test/x"
        );
        assert_eq!(redact(&"a1".repeat(15)), "<redacted>");
        assert_eq!(redact("/users/abc"), "/users/abc");
        // The cut lands inside a multibyte char: the result still fits and is valid.
        let long = format!("{}{}", "a".repeat(254), "é".repeat(10));
        let cut = cap_text(long);
        assert!(cut.len() <= STR_CALL_TEXT_MAX);
        assert!(cut.ends_with('…'));
        assert_eq!(cap_text("short".into()), "short");

        let key = |k: &str| ExportKey {
            namespace: "ts-export".into(),
            key: k.into(),
        };
        let calls = vec![StrCall {
            text: "app.get(\"/u\", show)".into(),
            span: span(3),
            refs: vec![
                (
                    "show".into(),
                    ArgRef::Candidates(vec![key("src/h.ts#show"), key("other")]),
                    true,
                ),
                ("auth".into(), ArgRef::Local("function:auth".into()), false),
                ("show".into(), ArgRef::Local("dup".into()), false),
            ],
        }];
        let fact = str_calls_fact(&calls).unwrap();
        let Fact::List(items) = &fact else { panic!() };
        assert_eq!(items.len(), 1);
        let (r, inf) = arg_ref_of(&items[0], "show").unwrap();
        assert!(inf);
        assert_eq!(
            r,
            ArgRef::Candidates(vec![key("src/h.ts#show"), key("other")])
        );
        assert_eq!(
            arg_ref_of(&items[0], "auth"),
            Some((ArgRef::Local("function:auth".into()), false))
        );
        assert_eq!(arg_ref_of(&items[0], "nope"), None);

        assert_eq!(str_calls_fact(&[]), None);
        let many: Vec<StrCall> = (0..300)
            .map(|i| StrCall {
                text: format!("f({i})"),
                span: span(i + 1),
                refs: vec![],
            })
            .collect();
        let Some(Fact::List(l)) = str_calls_fact(&many) else {
            panic!()
        };
        assert_eq!(l.len(), STR_CALLS_MAX);
        let Fact::Map(m) = &l[0] else { panic!() };
        assert!(!m.contains_key("refs"));

        let refs: Vec<_> = (0..20)
            .map(|i| {
                (
                    format!("h{i}"),
                    ArgRef::Local(format!("function:h{i}")),
                    false,
                )
            })
            .collect();
        let Some(Fact::List(l)) = str_calls_fact(&[StrCall {
            text: "f()".into(),
            span: span(1),
            refs,
        }]) else {
            panic!()
        };
        let Fact::Map(m) = &l[0] else { panic!() };
        let Some(Fact::Map(r)) = m.get("refs") else {
            panic!()
        };
        assert_eq!(r.len(), ARG_REFS_MAX);

        // A shard round-trip of OutRef with and without `binds`.
        let mk = |binds| OutRef {
            from: Id::of("core:a#fn:f"),
            kind: LinkKind("provides".into()),
            span: None,
            candidates: vec![key("k")],
            confidence: Confidence::Declared,
            origin: Some("profile:p#r".into()),
            binds,
        };
        let plain = mk(None);
        let json = serde_json::to_string(&plain).unwrap();
        assert!(!json.contains("binds"));
        assert_eq!(serde_json::from_str::<OutRef>(&json).unwrap(), plain);
        let bound = mk(Some(RefBinding {
            contract: ContractName {
                kind: "res".into(),
                key: "x".into(),
            },
            role: ContractRole::Provides,
        }));
        let json = serde_json::to_string(&bound).unwrap();
        assert!(json.contains("binds"));
        assert_eq!(serde_json::from_str::<OutRef>(&json).unwrap(), bound);
    }
}
