//! Working-set ranking tests (FEAT-039): tiers from a real checkout, the boost in `search` and
//! `map`, neutrality, and the view ID. They run the built binary against synthetic repos in
//! temp directories (the `.tx` test extractor and the fixture board).

mod working_set {
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    const BIN: &str = env!("CARGO_BIN_EXE_katalogr");

    fn run(dir: &Path, args: &[&str]) -> Output {
        Command::new(BIN)
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .output()
            .expect("run katalogr")
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let out = run(dir, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn json(dir: &Path, args: &[&str]) -> Value {
        serde_json::from_str(&ok(dir, args)).expect("json")
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=Example Author",
                "-c",
                "user.email=a@example.test",
            ])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let (src, dst) = (e.path(), to.join(e.file_name()));
            if src.is_dir() {
                copy_dir(&src, &dst);
            } else {
                std::fs::copy(&src, &dst).unwrap();
            }
        }
    }

    struct Fx {
        _tmp: tempfile::TempDir,
        code: PathBuf,
    }

    /// A git repo `code` on `main` with `a.tx`, `b.tx`, `c.tx` that each define `twin` (equal
    /// scores, so only the name breaks the tie), committed and indexed. With `board`, the
    /// fixture board is a second repo of the project.
    fn fixture(board: bool) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let code = base.join("code");
        std::fs::create_dir_all(&code).unwrap();
        git(&code, &["init", "-q"]);
        for f in ["a", "b", "c"] {
            std::fs::write(code.join(format!("{f}.tx")), "sym fn twin\n").unwrap();
        }
        std::fs::write(code.join("a.tx"), "sym fn twin\nsym fn only_here\n").unwrap();
        git(&code, &["add", "-A"]);
        git(&code, &["commit", "-q", "-m", "initial"]);
        let kx = base.join("kx");
        ok(&base, &["kindex", "init", kx.to_str().unwrap()]);
        let mut args: Vec<String> = [
            "--kindex",
            kx.to_str().unwrap(),
            "project",
            "init",
            "demo",
            "--mark",
        ]
        .map(String::from)
        .to_vec();
        args.extend(["--repo".into(), format!("code={}", code.display())]);
        if board {
            let b = base.join("board");
            copy_dir(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/kanbanr/board"),
                &b,
            );
            std::fs::remove_file(b.join("expected.yaml")).unwrap();
            args.extend(["--repo".into(), format!("board={}", b.display())]);
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        ok(&base, &refs);
        ok(&code, &["index", "--full"]);
        Fx { _tmp: tmp, code }
    }

    fn paths(v: &Value) -> Vec<String> {
        v["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["symbol"]["path"].as_str().unwrap().to_string())
            .collect()
    }

    fn dirty_c(f: &Fx) {
        std::fs::write(f.code.join("c.tx"), "sym fn twin\nsym fn extra\n").unwrap();
    }

    #[test]
    fn dirty_tier_from_position() {
        let f = fixture(false);
        std::fs::write(f.code.join("a.tx"), "sym fn twin\nsym fn changed\n").unwrap();
        std::fs::write(f.code.join("new.tx"), "sym fn fresh\n").unwrap();
        let v = json(&f.code, &["search", "twin", "--json"]);
        assert_eq!(v["working_set"]["dirty"], 2, "{v}");
        assert_eq!(v["working_set"]["branch"], 0);
        assert_eq!(v["working_set"]["recent"], 0);
    }

    #[test]
    fn branch_tier_from_merge_base() {
        let f = fixture(false);
        git(&f.code, &["checkout", "-q", "-b", "feat/topic"]);
        std::fs::write(f.code.join("b.tx"), "sym fn twin\nsym fn on_branch\n").unwrap();
        git(&f.code, &["commit", "-q", "-am", "work on the branch"]);
        let v = json(&f.code, &["search", "twin", "--json"]);
        assert_eq!(v["working_set"]["branch"], 1, "{v}");
        assert_eq!(v["working_set"]["dirty"], 0);
        assert_eq!(paths(&v)[0], "b.tx");
        // On the default branch itself, nothing differs from the merge-base.
        git(&f.code, &["checkout", "-q", "main"]);
        let v = json(&f.code, &["search", "twin", "--json"]);
        assert!(v.get("working_set").is_none(), "{v}");
    }

    #[test]
    fn recent_tier_from_edit_log() {
        let f = fixture(false);
        use std::io::Write;
        let input = serde_json::json!({
            "cwd": f.code.to_str().unwrap(),
            "tool_name": "Edit",
            "tool_input": {"file_path": f.code.join("b.tx").to_str().unwrap()},
        })
        .to_string();
        let mut child = Command::new(BIN)
            .current_dir(&f.code)
            .args(["hook", "post-edit"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
        let v = json(&f.code, &["search", "twin", "--json"]);
        assert_eq!(v["working_set"]["recent"], 1, "{v}");
        assert_eq!(paths(&v)[0], "b.tx");
    }

    fn link_c_to_item(f: &Fx) {
        std::fs::write(
            f.code.join("c.tx"),
            "sym fn twin\nref twin refs-item board-item:FEAT-001\n",
        )
        .unwrap();
        git(&f.code, &["commit", "-q", "-am", "link c to the item"]);
        ok(&f.code, &["index", "--full"]);
    }

    #[test]
    fn item_tier_from_branch_name() {
        let f = fixture(true);
        git(&f.code, &["checkout", "-q", "-b", "feat/FEAT-001-thing"]);
        link_c_to_item(&f);
        // The commit is on the branch, so c.tx is also in the branch tier; the strongest wins
        // and each path is counted once.
        git(&f.code, &["checkout", "-q", "main"]);
        git(
            &f.code,
            &["merge", "-q", "--ff-only", "feat/FEAT-001-thing"],
        );
        git(&f.code, &["checkout", "-q", "-b", "feat/FEAT-001-more"]);
        let v = json(&f.code, &["search", "twin", "--json"]);
        assert_eq!(v["working_set"]["item"], "FEAT-001", "{v}");
        // The code file and the board document that mentions the item.
        assert_eq!(v["working_set"]["item_paths"], 2);
        assert_eq!(paths(&v)[0], "c.tx");
    }

    #[test]
    fn no_board_no_item_tier() {
        let f = fixture(false);
        git(&f.code, &["checkout", "-q", "-b", "feat/FEAT-001-thing"]);
        let v = json(&f.code, &["search", "twin", "--json"]);
        assert!(v.get("working_set").is_none(), "{v}");
        assert_eq!(paths(&v), ["a.tx", "b.tx", "c.tx"]);
    }

    #[test]
    fn item_paths_capped_partial() {
        let f = fixture(true);
        for i in 0..520 {
            std::fs::write(
                f.code.join(format!("n{i:03}.tx")),
                format!("sym fn linked{i:03}\nref linked{i:03} refs-item board-item:FEAT-001\n"),
            )
            .unwrap();
        }
        git(&f.code, &["add", "-A"]);
        git(&f.code, &["commit", "-q", "-m", "many links"]);
        git(&f.code, &["checkout", "-q", "-b", "feat/FEAT-001-wide"]);
        ok(&f.code, &["index", "--full"]);
        let out = run(&f.code, &["search", "twin", "--json"]);
        assert!(out.status.success());
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["working_set"]["item_paths"], 500, "{v}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("partial: working set holds the first 500"),
            "{err}"
        );
    }

    #[test]
    fn search_favours_dirty_file() {
        let f = fixture(false);
        let clean = json(&f.code, &["search", "twin", "--json"]);
        assert_eq!(paths(&clean), ["a.tx", "b.tx", "c.tx"]);
        dirty_c(&f);
        let v = json(&f.code, &["search", "twin", "--json"]);
        assert_eq!(paths(&v)[0], "c.tx", "{v}");
        let neutral = json(&f.code, &["search", "twin", "--neutral", "--json"]);
        assert_eq!(paths(&neutral), ["a.tx", "b.tx", "c.tx"]);
        // The printed score stays the true one.
        assert_eq!(v["result"][0]["score"], neutral["result"][2]["score"]);
    }

    #[test]
    fn map_order_changes_rank_value_does_not() {
        let f = fixture(false);
        dirty_c(&f);
        let boosted = json(&f.code, &["map", "--json"]);
        let neutral = json(&f.code, &["map", "--neutral", "--json"]);
        assert_ne!(paths(&boosted), paths(&neutral));
        assert_eq!(boosted["result"][0]["symbol"]["path"], "c.tx");
        let ranks = |v: &Value| -> Vec<(String, String)> {
            let mut r: Vec<_> = v["result"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    (
                        e["symbol"]["name"].as_str().unwrap().to_string(),
                        e["rank"].to_string(),
                    )
                })
                .collect();
            r.sort();
            r
        };
        assert_eq!(ranks(&boosted), ranks(&neutral));
    }

    #[test]
    fn boost_never_adds_a_hit() {
        let f = fixture(false);
        dirty_c(&f);
        let set = |v: &Value| {
            let mut p = paths(v);
            p.sort();
            p
        };
        let boosted = json(&f.code, &["search", "twin", "--limit", "50", "--json"]);
        let neutral = json(
            &f.code,
            &["search", "twin", "--limit", "50", "--neutral", "--json"],
        );
        assert_eq!(set(&boosted), set(&neutral));
        // `extra` does not match, and being dirty does not make it match.
        assert!(!set(&boosted).is_empty());
        let none = run(&f.code, &["search", "nomatchword", "--json"]);
        assert!(!none.status.success());
        // The limit applies after the boost.
        let one = json(&f.code, &["search", "twin", "--limit", "1", "--json"]);
        assert_eq!(paths(&one), ["c.tx"]);
        let one = json(
            &f.code,
            &["search", "twin", "--limit", "1", "--neutral", "--json"],
        );
        assert_eq!(paths(&one), ["a.tx"]);
    }

    #[test]
    fn empty_set_changes_nothing_end_to_end() {
        let f = fixture(false);
        let a = ok(&f.code, &["search", "twin", "--json"]);
        let b = ok(&f.code, &["search", "twin", "--neutral", "--json"]);
        assert_eq!(a, b);
        let a = ok(&f.code, &["map", "--json"]);
        let b = ok(&f.code, &["map", "--neutral", "--json"]);
        assert_eq!(a, b);
    }

    #[test]
    fn neutral_equals_no_set() {
        let f = fixture(false);
        let before = json(&f.code, &["search", "twin", "--neutral", "--json"]);
        dirty_c(&f);
        let after = json(&f.code, &["search", "twin", "--neutral", "--json"]);
        assert_eq!(paths(&before), paths(&after));
        assert!(after.get("working_set").is_none());
        let clean = fixture(false);
        assert_eq!(
            ok(&clean.code, &["search", "twin", "--json"]),
            ok(&clean.code, &["search", "twin", "--neutral", "--json"])
        );
    }

    #[test]
    fn answer_names_the_set() {
        let f = fixture(false);
        dirty_c(&f);
        let v = json(&f.code, &["search", "twin", "--json"]);
        let ws = &v["working_set"];
        assert_eq!(ws["dirty"], 1, "{v}");
        assert_eq!(ws["recent"], 0);
        assert_eq!(ws["branch"], 0);
        assert_eq!(ws["item_paths"], 0);
        assert!(ws["item"].is_null());
    }

    #[test]
    fn same_inputs_same_bytes() {
        let f = fixture(false);
        dirty_c(&f);
        let a = ok(&f.code, &["search", "twin", "--json"]);
        let b = ok(&f.code, &["search", "twin", "--json"]);
        assert_eq!(a, b);
        let a = ok(&f.code, &["map", "--json"]);
        let b = ok(&f.code, &["map", "--json"]);
        assert_eq!(a, b);
    }

    #[test]
    fn at_is_neutral() {
        let f = fixture(false);
        dirty_c(&f);
        let pos = json(&f.code, &["position", "--json"]);
        let live = pos["live"]["text"]
            .as_str()
            .or_else(|| pos["text"].as_str())
            .unwrap()
            .to_string();
        let v = json(&f.code, &["--at", &live, "search", "twin", "--json"]);
        assert_eq!(paths(&v), ["a.tx", "b.tx", "c.tx"], "{v}");
        assert!(v.get("working_set").is_none());
    }

    #[test]
    fn view_id_includes_set_hash() {
        let f = fixture(false);
        let neutral = json(&f.code, &["search", "twin", "--neutral", "--json"]);
        dirty_c(&f);
        let a = json(&f.code, &["search", "twin", "--json"]);
        let a2 = json(&f.code, &["search", "twin", "--json"]);
        let n2 = json(&f.code, &["search", "twin", "--neutral", "--json"]);
        let id = |v: &Value| v["view_id"].as_str().map(str::to_string);
        assert!(id(&a).is_some(), "{a}");
        assert_eq!(id(&a), id(&a2));
        assert_ne!(id(&a), id(&n2));
        assert!(id(&neutral).is_some());
        // A different set gives a different ID.
        std::fs::write(f.code.join("c.tx"), "sym fn twin\n").unwrap();
        std::fs::write(f.code.join("b.tx"), "sym fn twin\nsym fn other\n").unwrap();
        let b = json(&f.code, &["search", "twin", "--json"]);
        assert_ne!(id(&a), id(&b));
    }
}
