//! `katalogr view` through the CLI (FEAT-088): the answer, the store, `--fresh` and `--at`.

#[path = "views_support/mod.rs"]
mod support;

mod views {
    use super::support::*;
    use serde_json::Value;

    /// R-8. `--json` prints a `ViewAnswer` that validates against `schema/v1/view.schema.json`,
    /// and the text form prints the Mermaid.
    #[test]
    fn cli_view_json_validates() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let v = view_json(&repo, &["structure", "--level", "file"]);
        let schema = view_schema();
        validate(&schema, &schema, &v, "$").unwrap_or_else(|e| panic!("{e}\n{v}"));
        assert_eq!(v["exact"], true);
        assert!(v["view_id"].as_str().unwrap().starts_with("v1-"), "{v}");
        assert!(v["mermaid"].as_str().unwrap().starts_with("flowchart"));
        assert_eq!(v["query"]["op"], "structure");
        assert_eq!(v["query"]["args"]["level"], "file");
        assert_eq!(v["pele"], "1.7.0");
        assert!(v["graph"]["nodes"].as_array().is_some_and(|n| !n.is_empty()));
        assert!(v.get("watermark").is_none());
        let text = ok(&repo, &["view", "structure", "--level", "file"]);
        assert_eq!(text.trim_end(), v["mermaid"].as_str().unwrap().trim_end());
        // Not exact (a question not asked before, so no hit): marked, with the read's marks
        // and no ID.
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
        validate(&schema, &schema, &n, "$").unwrap_or_else(|e| panic!("{e}\n{n}"));
        assert_eq!(n["exact"], false);
        assert!(n.get("view_id").is_none());
        for k in ["watermark", "pending", "stale"] {
            assert!(n.get(k).is_some(), "{k} in {n}");
        }
    }

    /// R-8. A view needs a project, not a board.
    #[test]
    fn runs_without_board() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let empty = tempfile::tempdir().unwrap();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(&repo)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env_remove("KANBANR_DATA_DIR")
            .env("PATH", empty.path())
            .env("HOME", empty.path())
            .args(["--json", "view", "structure"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{out:?}");
    }

    /// R-11. A fresh computation of an untouched entry is the same, and exits 0.
    #[test]
    fn fresh_matches_stored() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let first = cli(&repo, &["view", "--fresh", "structure"]);
        assert_eq!(first.status.code(), Some(0), "{first:?}");
        assert!(String::from_utf8_lossy(&first.stdout).contains("not-stored"));
        view_json(&repo, &["structure"]);
        for args in [
            ["view", "--fresh", "structure"],
            ["view", "structure", "--fresh"],
        ] {
            let out = cli(&repo, &args);
            assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
            assert!(String::from_utf8_lossy(&out.stdout).contains("same"));
        }
    }

    /// R-11. A stored entry that a fresh computation does not reproduce exits 1, naming the ID.
    #[test]
    fn fresh_mismatch_exits_nonzero() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = project(tmp.path());
        let v = view_json(&repo, &["structure"]);
        let id = v["view_id"].as_str().unwrap().to_string();
        let mut doctored = v.clone();
        doctored["mermaid"] = "flowchart LR\n  old".into();
        std::fs::write(stored_path(&kx, &id), serde_json::to_vec(&doctored).unwrap()).unwrap();
        let out = cli(&repo, &["view", "--fresh", "structure"]);
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains(&id), "{out:?}");
    }

    /// R-1. `--at` answers for a position; the answer reports that position.
    #[test]
    fn at_param_answers() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let pos: Value = serde_json::from_str(&ok(&repo, &["--json", "position"])).unwrap();
        let text = pos["text"].as_str().unwrap().to_string();
        let v = view_json(&repo, &["structure", "--at", &text]);
        assert_eq!(v["exact"], true, "{v}");
        assert_eq!(v["positions"][0]["commit"], pos["live"]["commit"]);
        // The same position, asked before the flags of the view, is the same answer.
        let w: Value = serde_json::from_str(&ok(
            &repo,
            &["--json", "--at", &text, "view", "structure"],
        ))
        .unwrap();
        assert_eq!(w["view_id"], v["view_id"]);
        // A position nobody holds is exit 6.
        let none = "0".repeat(40);
        let out = cli(&repo, &["--json", "view", "structure", "--at", &none]);
        assert_eq!(out.status.code(), Some(6), "{out:?}");
    }

    /// R-1. A query block's text and the named parameters, defaults left out or written, are one
    /// question with one ID.
    #[test]
    fn block_and_params_same_id() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let plain = view_json(&repo, &["structure"]);
        let spelled = view_json(
            &repo,
            &[
                "structure",
                "--inferred=false",
                "--max-nodes",
                "60",
                "--level=package",
            ],
        );
        assert!(plain["view_id"].is_string());
        assert_eq!(plain["view_id"], spelled["view_id"]);
        assert_eq!(plain["query"], spelled["query"]);
        let other = view_json(&repo, &["structure", "--level", "file"]);
        assert_ne!(plain["view_id"], other["view_id"]);
    }
}
