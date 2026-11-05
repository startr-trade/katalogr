//! FEAT-084 T1: the text form of a position.

mod position {
    use katalogr_query::PositionRef;

    const C: &str = "e410760849abb8cd9fa8fbba305f5dd5d96ba954";
    const D: &str = "83ebb47e03fee94773edcd621a87f37980b3b408b96467062fcc87368f3260c7";

    #[test]
    fn text_roundtrip() {
        for (text, commit, dirty) in [
            (C.to_string(), Some(C), None),
            (format!("{C}+{D}"), Some(C), Some(D)),
            (format!("none+{D}"), None, Some(D)),
        ] {
            let p = PositionRef::parse("r", &text).unwrap();
            assert_eq!(p.repo, "r");
            assert_eq!(p.commit.as_deref(), commit);
            assert_eq!(p.dirty.as_deref(), dirty);
            assert_eq!(p.text(), text);
        }
        let p = PositionRef::parse("r", &format!("{C}+{D}")).unwrap();
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            format!(r#"{{"repo":"r","commit":"{C}","dirty":"{D}"}}"#)
        );
        let none = PositionRef {
            repo: "r".into(),
            commit: None,
            dirty: Some(D.into()),
        };
        assert!(
            serde_json::to_string(&none)
                .unwrap()
                .contains(r#""commit":null"#)
        );
    }

    #[test]
    fn bad_text_is_error() {
        let upper = C.to_uppercase();
        for bad in [
            "",
            "none",
            "abc",
            &C[..39],
            &format!("{C}0"),
            &upper,
            &format!("{C}+"),
            &format!("{C}+{}", &D[..63]),
            &format!("{C}+{}", D.to_uppercase()),
            &format!("none+{}", &D[..10]),
            &format!("{C}+{D}+{D}"),
            &format!("zz{}+{D}", &C[2..]),
        ] {
            assert!(PositionRef::parse("r", bad).is_err(), "accepted {bad:?}");
        }
    }
}
