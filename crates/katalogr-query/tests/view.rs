//! FEAT-084 T5: the format of a view ID.

mod view {
    use katalogr_query::{PositionRef, ViewQuery, view_id};

    fn q(op: &str, args: &[(&str, &str)]) -> ViewQuery {
        ViewQuery {
            op: op.to_string(),
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// R-6. The ID of a fixed input, computed outside this code from the spec's rule: the first
    /// 32 hex of the SHA-256 of the compact JSON with sorted keys.
    #[test]
    fn id_format_spec_vector() {
        let positions = [
            PositionRef {
                repo: "app".into(),
                commit: Some("e410760849abb8cd9fa8fbba305f5dd5d96ba954".into()),
                dirty: None,
            },
            PositionRef {
                repo: "zed".into(),
                commit: None,
                dirty: Some(
                    "83ebb47e03fee94773edcd621a87f37980b3b408b96467062fcc87368f3260c7".into(),
                ),
            },
        ];
        let id = view_id(
            &positions,
            "0123abcd",
            &q("find", &[("text", "call"), ("budget", "400")]),
        );
        assert_eq!(id, "v1-096ec1a42f3e9b74ae0f0348152caeb2");
        // Argument order is irrelevant; a different argument is a different ID.
        let same = view_id(
            &positions,
            "0123abcd",
            &q("find", &[("budget", "400"), ("text", "call")]),
        );
        assert_eq!(same, id);
        assert_ne!(
            view_id(&positions, "0123abcd", &q("find", &[("text", "other")])),
            id
        );
        assert_ne!(view_id(&positions, "other", &q("find", &[])), id);
        assert_ne!(view_id(&positions[..1], "0123abcd", &q("find", &[])), id);
    }
}
