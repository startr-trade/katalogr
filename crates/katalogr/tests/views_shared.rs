//! `fixtures/contract/views-v1.json` (FEAT-088 R-6): what a client may rely on, shared with
//! kanbanr, which carries a copy. A real answer must have the fields, the order and the types
//! the file records, so a change shows in either tool's tests.

#[path = "views_support/mod.rs"]
mod support;

mod views {
    use super::support::*;
    use serde_json::Value;
    use std::path::Path;

    fn shared() -> Value {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/contract/views-v1.json");
        serde_json::from_str(&std::fs::read_to_string(path).expect("read views-v1.json"))
            .expect("parse views-v1.json")
    }

    fn keys(v: &Value) -> Vec<&str> {
        v.as_object().unwrap().keys().map(String::as_str).collect()
    }

    fn strings(v: &Value) -> Vec<&str> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect()
    }

    /// The JSON type of a value, for comparing a real answer with a sample.
    fn kind(v: &Value) -> &'static str {
        match v {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    /// R-6. The samples are well formed, a real answer matches them field for field, and the
    /// required fields are the ones the samples hold.
    #[test]
    fn shared_file_matches() {
        let file = shared();
        assert_eq!(file["format"], 1);
        let (exact, not_exact) = (&file["exact"], &file["not_exact"]);

        // The required lists against the samples.
        let always = strings(&file["required"]["always"]);
        let when_exact = strings(&file["required"]["when_exact"]);
        let when_not = strings(&file["required"]["when_not_exact"]);
        for f in always.iter().chain(&when_exact) {
            assert!(exact.get(*f).is_some(), "exact sample lacks {f}");
        }
        for f in always.iter().chain(&when_not) {
            assert!(not_exact.get(*f).is_some(), "not-exact sample lacks {f}");
        }
        for f in &when_not {
            assert!(exact.get(*f).is_none(), "exact sample has {f}");
        }
        for f in &when_exact {
            assert!(not_exact.get(*f).is_none(), "not-exact sample has {f}");
        }
        assert_eq!(exact["exact"], true);
        assert_eq!(not_exact["exact"], false);

        // The samples validate against the schema.
        let schema = view_schema();
        for (name, s) in [("exact", exact), ("not_exact", not_exact)] {
            validate(&schema, &schema, s, "$").unwrap_or_else(|e| panic!("{name}: {e}"));
        }

        // A real exact answer: same fields in the same order, same types, same content apart
        // from the position and the ID.
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let real = view_json(&repo, &["structure", "--level", "file"]);
        assert_eq!(keys(&real), keys(exact), "exact: fields or their order");
        for (k, v) in real.as_object().unwrap() {
            assert_eq!(kind(v), kind(&exact[k]), "exact: type of {k}");
        }
        for k in ["query", "mermaid", "graph", "pele", "schema", "view_format"] {
            assert_eq!(real[k], exact[k], "exact: {k}");
        }
        let id = real["view_id"].as_str().unwrap();
        assert!(
            id.len() == 35
                && id.starts_with("v1-")
                && id[3..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{id}"
        );
        assert_eq!(keys(&real["positions"][0]), keys(&exact["positions"][0]));

        // A real answer that is not exact.
        let n = view_json(
            &repo,
            &[
                "structure",
                "--level",
                "file",
                "--max-nodes",
                "59",
                "--consistency",
                "stale-ok",
            ],
        );
        assert_eq!(
            keys(&n),
            keys(not_exact),
            "not exact: fields or their order"
        );
        for (k, v) in n.as_object().unwrap() {
            assert_eq!(kind(v), kind(&not_exact[k]), "not exact: type of {k}");
        }
    }
}
