//! The embedding SPI helpers (FEAT-017 R-14, R-24).

mod embed {
    use katalogr_model::*;
    use std::collections::BTreeMap;

    fn sym(path: &str, segs: &[(&str, &str)], span: (u32, u32, u32, u32)) -> SymbolNode {
        SymbolNode {
            name: SymbolName {
                artifact: ArtifactName::new("r", path).unwrap(),
                segments: segs
                    .iter()
                    .map(|(k, n)| Segment {
                        kind: (*k).into(),
                        name: (*n).into(),
                    })
                    .collect(),
                ordinal: 1,
            },
            kind: segs.last().unwrap().0.into(),
            span: Span {
                start_line: span.0,
                start_col: span.1,
                end_line: span.2,
                end_col: span.3,
            },
            facts: BTreeMap::new(),
        }
    }

    fn at(l: u32, c: u32) -> Span {
        Span {
            start_line: l,
            start_col: c,
            end_line: l,
            end_col: c + 3,
        }
    }

    fn item(text: &str, call: Option<CallContext>) -> EmbeddedText {
        EmbeddedText {
            owner: Id::of("x"),
            span: at(1, 1),
            text: text.into(),
            holes: vec![],
            call,
        }
    }

    #[test]
    fn innermost_owner_picks_smallest_span() {
        let artifact = ArtifactName::new("r", "a.rs").unwrap().id();
        let imp = sym("a.rs", &[("impl", "T")], (1, 1, 20, 2));
        let f = sym("a.rs", &[("impl", "T"), ("fn", "run")], (3, 5, 9, 6));
        let inner = sym(
            "a.rs",
            &[("impl", "T"), ("fn", "run"), ("fn", "helper")],
            (5, 9, 7, 10),
        );
        let syms = vec![imp.clone(), f.clone(), inner.clone()];
        // nested fn inside an impl owns
        assert_eq!(innermost_owner(&syms, artifact, at(6, 10)), inner.name.id());
        assert_eq!(innermost_owner(&syms, artifact, at(4, 6)), f.name.id());
        assert_eq!(innermost_owner(&syms, artifact, at(15, 6)), imp.name.id());
        // no symbol contains it: the artifact
        assert_eq!(innermost_owner(&syms, artifact, at(30, 1)), artifact);
        assert_eq!(innermost_owner(&[], artifact, at(1, 1)), artifact);
        // equal spans resolve by the smallest canonical name
        let a = sym("a.rs", &[("fn", "a")], (2, 1, 4, 2));
        let b = sym("a.rs", &[("fn", "b")], (2, 1, 4, 2));
        let both = vec![b.clone(), a.clone()];
        assert_eq!(innermost_owner(&both, artifact, at(3, 1)), a.name.id());
    }

    #[test]
    fn offer_applies_limits() {
        let mut out = Vec::new();
        // without a call: ok, too short, no whitespace, empty, too long
        assert!(offer(&mut out, item("select a from t", None)));
        assert!(!offer(&mut out, item("a b", None)));
        assert!(!offer(&mut out, item("nowhitespacehere", None)));
        assert!(!offer(&mut out, item("        ", None)));
        assert!(!offer(
            &mut out,
            item(&format!("a {}", "x".repeat(EMBED_MAX_LEN)), None)
        ));
        assert_eq!(out.len(), 1);
        // with a call: a short path without whitespace is accepted
        let call = CallContext::new("client.get", 0);
        assert!(offer(&mut out, item("/orders", call.clone())));
        // an all-space text and an over-limit text are refused
        assert!(!offer(&mut out, item("   ", call.clone())));
        assert!(!offer(
            &mut out,
            item(&"y".repeat(EMBED_MAX_LEN + 1), call.clone())
        ));
        assert_eq!(out.len(), 2);
        // exactly at the limit is accepted
        let mut full = Vec::new();
        for _ in 0..EMBED_MAX_PER_FILE {
            assert!(offer(&mut full, item("select a from t", None)));
        }
        // the 2,001st offer is refused
        assert!(!offer(&mut full, item("select a from t", None)));
        assert!(!offer(&mut full, item("/orders", call)));
        assert_eq!(full.len(), EMBED_MAX_PER_FILE);
    }

    #[test]
    fn call_context_normalised() {
        let c = CallContext::new("reqwest :: get", 2).unwrap();
        assert_eq!(c.callee, "reqwest::get");
        assert_eq!(c.arg, 2);
        assert!(CallContext::new("  ", 0).is_none());
        assert!(CallContext::new("", 0).is_none());
        // 255 ASCII bytes, then a two-byte char straddling byte 256: cut before it
        let long = format!("{}é{}", "a".repeat(255), "b".repeat(40));
        let c = CallContext::new(&long, 0).unwrap();
        assert_eq!(c.callee.len(), 255);
        assert!(c.callee.chars().all(|ch| ch == 'a'));
        // exactly CALL_CALLEE_MAX bytes of ASCII is kept whole
        let c = CallContext::new(&"a".repeat(300), 0).unwrap();
        assert_eq!(c.callee.len(), CALL_CALLEE_MAX);
        // a constructor keeps the one space after `new`
        assert_eq!(CallContext::new("new URL", 0).unwrap().callee, "new URL");
    }
}
