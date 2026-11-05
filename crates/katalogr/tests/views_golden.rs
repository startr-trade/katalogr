//! The golden output of every view op (FEAT-088 R-5). A stored view is trusted by its ID alone,
//! and the ID covers the position, the facts and the question, but not this binary's queries or
//! renderers. So a change that alters what a view looks like must raise `view_format`, and this
//! test fails until it does.

#[path = "views_support/mod.rs"]
mod support;

mod views {
    use super::support::*;
    use serde_json::{Value, json};
    use std::path::Path;

    /// The views the fixture holds, as the words after `katalogr view`.
    const CASES: &[&str] = &[
        "structure",
        "structure --level file",
        "contracts",
        "callers target",
        "path top caller_a",
        "cycles --level file",
    ];

    fn fixture_path() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/contract/views-golden-v1.json")
    }

    fn words(line: &str) -> Vec<&str> {
        line.split_whitespace().collect()
    }

    /// R-5. Each op on the fixed synthetic repository gives the recorded Mermaid and graph.
    #[test]
    fn format_golden() {
        let path = fixture_path();
        if std::env::var_os("KATALOGR_BLESS").is_some_and(|v| v == "1") {
            let tmp = tempfile::tempdir().unwrap();
            let (_kx, repo) = project(tmp.path());
            let files: serde_json::Map<String, Value> = FILES
                .iter()
                .map(|(n, t)| (n.to_string(), json!(t)))
                .collect();
            let cases: Vec<Value> = CASES
                .iter()
                .map(|line| {
                    let v = view_json(&repo, &words(line));
                    json!({"query": line, "mermaid": v["mermaid"], "graph": v["graph"]})
                })
                .collect();
            let doc = json!({"format": 1, "files": files, "cases": cases});
            let mut text = serde_json::to_string_pretty(&doc).unwrap();
            text.push('\n');
            std::fs::write(&path, text).unwrap();
        }
        let golden: Value = serde_json::from_str(
            &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
        )
        .unwrap();
        let files: Vec<(String, String)> = golden["files"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(n, t)| (n.clone(), t.as_str().unwrap().to_string()))
            .collect();
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect();
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project_with(tmp.path(), &files);
        let cases = golden["cases"].as_array().unwrap();
        assert_eq!(cases.len(), CASES.len(), "the fixture lacks a case");
        for case in cases {
            let line = case["query"].as_str().unwrap();
            let v = view_json(&repo, &words(line));
            assert!(
                v["mermaid"] == case["mermaid"] && v["graph"] == case["graph"],
                "the output of `view {line}` changed.\n\
                 A stored view is trusted by its ID alone, so if this change is meant, raise \
                 `view_format` (VIEW_FORMAT in katalogr-query and in katalogr-dist's contract, \
                 and the `v1-` prefix of the ID) and then re-record this file with \
                 KATALOGR_BLESS=1.\n\
                 expected mermaid:\n{}\nactual mermaid:\n{}\nexpected graph: {}\nactual graph: {}",
                case["mermaid"].as_str().unwrap_or_default(),
                v["mermaid"].as_str().unwrap_or_default(),
                case["graph"],
                v["graph"],
            );
        }
    }
}
