//! FEAT-034: `katalogr export graph` against a synthetic `.tx` project.

#[path = "views_support/mod.rs"]
mod support;

mod export_graph {
    use super::support::*;
    use serde_json::Value;
    use std::path::{Path, PathBuf};

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        (tmp, repo)
    }

    fn export(repo: &Path, out: &Path, format: &str, view: &[&str]) -> std::process::Output {
        let mut args = vec!["export", "graph"];
        args.extend_from_slice(view);
        args.extend_from_slice(&["--format", format, "--out", out.to_str().unwrap()]);
        cli(repo, &args)
    }

    #[test]
    fn dot_equals_render() {
        let (tmp, repo) = setup();
        let out = tmp.path().join("g.dot");
        let r = export(&repo, &out, "dot", &["structure", "--level", "file"]);
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        let rendered = ok(
            &repo,
            &["render", "structure", "--level", "file", "--format", "dot"],
        );
        assert_eq!(std::fs::read_to_string(&out).unwrap(), rendered);
    }

    #[test]
    fn graphml_equals_render() {
        let (tmp, repo) = setup();
        let out = tmp.path().join("g.graphml");
        let r = export(&repo, &out, "graphml", &["structure", "--level", "file"]);
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        let rendered = ok(
            &repo,
            &[
                "render",
                "structure",
                "--level",
                "file",
                "--format",
                "graphml",
            ],
        );
        assert_eq!(std::fs::read_to_string(&out).unwrap(), rendered);
    }

    #[test]
    fn json_is_cytoscape_elements() {
        let (tmp, repo) = setup();
        let out = tmp.path().join("g.json");
        let r = export(&repo, &out, "json", &["structure", "--level", "file"]);
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(v["format"], "katalogr-graph");
        assert_eq!(v["format_version"], 1);
        let nodes = v["elements"]["nodes"].as_array().unwrap();
        let edges = v["elements"]["edges"].as_array().unwrap();
        assert!(!nodes.is_empty() && !edges.is_empty());
        let ids: Vec<&str> = nodes
            .iter()
            .map(|n| n["data"]["id"].as_str().unwrap())
            .collect();
        for e in edges {
            assert!(ids.contains(&e["data"]["source"].as_str().unwrap()));
            assert!(ids.contains(&e["data"]["target"].as_str().unwrap()));
        }
        // The view's own numbers are kept.
        let view = view_json(&repo, &["structure", "--level", "file"]);
        assert_eq!(v["shown"], view["graph"]["shown"]);
        assert_eq!(v["total"], view["graph"]["total"]);
    }

    #[test]
    fn same_position_same_bytes() {
        let (tmp, repo) = setup();
        for format in ["dot", "graphml", "json"] {
            let a = tmp.path().join(format!("a.{format}"));
            let b = tmp.path().join(format!("b.{format}"));
            assert!(export(&repo, &a, format, &["structure"]).status.success());
            assert!(export(&repo, &b, format, &["structure"]).status.success());
            assert_eq!(
                std::fs::read(&a).unwrap(),
                std::fs::read(&b).unwrap(),
                "{format}"
            );
        }
    }

    #[test]
    fn partial_warns_and_records() {
        let (tmp, repo) = setup();
        let out = tmp.path().join("g.json");
        let r = export(
            &repo,
            &out,
            "json",
            &["structure", "--level", "file", "--max-nodes", "2"],
        );
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        let err = String::from_utf8_lossy(&r.stderr);
        assert!(
            err.contains("partial") || err.contains("shows 2 of"),
            "{err}"
        );
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(v["shown"], 2);
        assert!(v["total"].as_u64().unwrap() > 2, "{v}");
    }
}
