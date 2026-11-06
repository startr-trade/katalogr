#![cfg(feature = "web")]
//! FEAT-101 R-14: an exported file embeds the lineage answers of the symbols it plans.

#[path = "views_support/mod.rs"]
mod support;

mod export_html {
    use super::support::*;
    use katalogr_web::export::{PlanOptions, answer_key, plan_symbol};
    use serde_json::Value;

    /// The `kw-data` block of a file.
    fn data_of(file: &str) -> Value {
        let tag = "<script type=\"application/json\" id=\"kw-data\">";
        let start = file.find(tag).expect("a data block") + tag.len();
        let end = file[start..].find("</script>").unwrap() + start;
        serde_json::from_str(&file[start..end]).expect("the data is JSON")
    }

    /// FEAT-101 R-14: every planned symbol has its `lineage` and `views/lineage` answers in the
    /// file, and the plan names them only when the capability is present.
    #[test]
    fn lineage_embedded_for_planned_symbols() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let out = tmp.path().join("a.html");
        let r = cli(&repo, &["export", "html", "--out", out.to_str().unwrap()]);
        assert!(
            r.status.success(),
            "export failed: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        let data = data_of(&std::fs::read_to_string(&out).unwrap());
        let caps: Vec<String> = data["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_string())
            .collect();
        assert!(caps.iter().any(|c| c == "type-lineage"), "{caps:?}");
        let symbols = data["symbols"].as_array().unwrap();
        assert!(!symbols.is_empty());
        let opts = PlanOptions { with_source: false };
        for node in symbols {
            let name = node["name"].as_str().unwrap();
            let planned = plan_symbol(name, "", 1, &opts, &caps);
            let keys: Vec<String> = planned
                .iter()
                .filter(|c| c.op == "lineage" || c.op == "views/lineage")
                .map(answer_key)
                .collect();
            assert_eq!(keys.len(), 2, "{name}: {keys:?}");
            for key in keys {
                assert!(data["answers"].get(&key).is_some(), "{name}: {key} missing");
            }
            let bare = plan_symbol(name, "", 1, &opts, &[]);
            assert!(bare.iter().all(|c| !c.op.contains("lineage")));
        }
    }
}
