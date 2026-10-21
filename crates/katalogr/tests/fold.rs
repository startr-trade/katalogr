//! Folding of very-high fan-out in `refs` and `callers` (FEAT-077, ADR-0028).

mod fold {
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .output()
            .expect("run katalogr");
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).expect("utf8"),
            String::from_utf8(out.stderr).expect("utf8"),
        )
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let (code, out, err) = run(dir, args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        out
    }

    fn json(repo: &Path, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = ok(repo, &all);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{all:?}: {e}: {out}"))
    }

    /// The text of an answer without its closing `next:` line.
    fn text(repo: &Path, args: &[&str]) -> String {
        let out = ok(repo, args);
        match out.rfind("\nnext: ") {
            Some(i) => out[..=i].to_string(),
            None => out,
        }
    }

    /// The fold fixture: `impls` exporters of `m:area`, 7 callers through the name, `direct`
    /// by a stored link, and `top` above `caller_0`.
    fn project(tmp: &Path, impls: usize, extra: &str) -> PathBuf {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        ok(tmp, &["kindex", "init", kx.to_str().unwrap()]);
        let spec = format!("app={}", repo.display());
        ok(
            tmp,
            &[
                "--kindex",
                kx.to_str().unwrap(),
                "project",
                "init",
                "demo",
                "--repo",
                &spec,
                "--mark",
            ],
        );
        let mut a = String::new();
        for i in 0..impls {
            a.push_str(&format!("sym fn impl_{i}\nexport m:area impl_{i}\n"));
        }
        // `direct` reaches `impl_0` by a stored link; `m:one` names `impl_0` alone.
        a.push_str("sym fn direct\nlink direct calls impl_0\nexport m:one impl_0\n");
        std::fs::write(repo.join("impls.tx"), a).unwrap();
        let mut b = String::new();
        for j in 0..7 {
            b.push_str(&format!("sym fn caller_{j}\nref caller_{j} calls m:area\n"));
        }
        b.push_str(extra);
        std::fs::write(repo.join("callers.tx"), b).unwrap();
        ok(&repo, &["index"]);
        repo
    }

    fn names(v: &Value) -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|h| {
                h["name"]
                    .as_str()
                    .unwrap()
                    .rsplit(':')
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn refs_folds_high_fanout_key() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), 60, "");
        let t = text(&repo, &["refs", "impl_0"]);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines.len(), 2, "{t}");
        // Hits sort by name: the callers file comes before the impls file, so the fold is first.
        assert!(lines[1].contains("direct"), "{t}");
        assert!(
            lines[0].starts_with("folded  calls  m:area  60 targets  7 refs  first "),
            "{t}"
        );
        for n in ["caller_0", "caller_1", "caller_2", "caller_3", "caller_4"] {
            assert!(lines[0].contains(n), "{t}");
        }
        assert!(!lines[0].contains("caller_5"), "{t}");
        let j = json(&repo, &["refs", "impl_0"]);
        assert_eq!(j["result"].as_array().unwrap().len(), 1);
        let f = &j["folded"][0];
        assert_eq!(
            (f["key"].as_str(), f["kind"].as_str()),
            (Some("m:area"), Some("calls"))
        );
        assert_eq!(
            (f["targets"].as_u64(), f["hits"].as_u64(), f["at"].as_u64()),
            (Some(60), Some(7), Some(0))
        );
        assert_eq!(f["first"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn refs_below_threshold_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), 49, "");
        let plain = ok(&repo, &["refs", "impl_0"]);
        assert_eq!(plain, ok(&repo, &["refs", "impl_0", "--expand"]));
        let j = ok(&repo, &["--json", "refs", "impl_0"]);
        assert_eq!(j, ok(&repo, &["--json", "refs", "impl_0", "--expand"]));
        assert!(!j.contains("\"folded\""));
    }

    #[test]
    fn refs_stored_entry_not_folded() {
        let tmp = tempfile::tempdir().unwrap();
        // `mixed` also reaches `impl_0` through a name that only it exports: a stored entry.
        let repo = project(
            tmp.path(),
            60,
            "sym fn mixed\nref mixed calls m:area\nref mixed calls m:one\n",
        );
        let j = json(&repo, &["refs", "impl_0"]);
        let kept = names(
            &j["result"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h["from"].clone())
                .collect::<Value>(),
        );
        assert!(kept.contains(&"mixed".to_string()), "{kept:?}");
        assert!(kept.contains(&"direct".to_string()), "{kept:?}");
        assert_eq!(j["folded"][0]["hits"].as_u64(), Some(7));
    }

    #[test]
    fn callers_folds_per_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), 60, "sym fn top\nlink top calls caller_0\n");
        let j = json(&repo, &["callers", "impl_0", "--depth", "2"]);
        let hits = j["result"].as_array().unwrap();
        let got: Vec<(String, u64)> = hits
            .iter()
            .map(|h| {
                (
                    h["caller"]["name"]
                        .as_str()
                        .unwrap()
                        .rsplit(':')
                        .next()
                        .unwrap()
                        .to_string(),
                    h["depth"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(got, [("direct".to_string(), 1), ("top".to_string(), 2)]);
        let folds = j["folded"].as_array().unwrap();
        assert_eq!(folds.len(), 1);
        assert_eq!(folds[0]["depth"].as_u64(), Some(1));
        assert_eq!(folds[0]["hits"].as_u64(), Some(7));
        let t = text(&repo, &["callers", "impl_0", "--depth", "2"]);
        assert!(
            t.contains("folded  calls  m:area  60 targets  7 callers  depth 1  first "),
            "{t}"
        );
    }

    #[test]
    fn callers_contract_steps_not_folded() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            60,
            "sym fn consumer\nbind consumer consumes channel:c\n",
        );
        let mut a = std::fs::read_to_string(repo.join("impls.tx")).unwrap();
        a.push_str("bind impl_0 provides channel:c\n");
        std::fs::write(repo.join("impls.tx"), a).unwrap();
        ok(&repo, &["index"]);
        let j = json(&repo, &["callers", "impl_0"]);
        let via: Vec<&str> = j["result"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|h| h["via"].is_string())
            .map(|h| h["caller"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(via.len(), 1, "{j}");
        assert!(via[0].ends_with("consumer"), "{via:?}");
        assert_eq!(j["folded"][0]["hits"].as_u64(), Some(7));
    }

    #[test]
    fn expand_equals_unfolded() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), 60, "");
        let t = text(&repo, &["refs", "impl_0", "--expand"]);
        assert_eq!(t.lines().count(), 8, "{t}");
        assert!(!t.contains("folded"), "{t}");
        let j = ok(&repo, &["--json", "refs", "impl_0", "--expand"]);
        assert!(!j.contains("\"folded\""), "{j}");
        let v: Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["result"].as_array().unwrap().len(), 8);
        let c = ok(&repo, &["--json", "callers", "impl_0", "--expand"]);
        assert!(!c.contains("\"folded\""), "{c}");
        let v: Value = serde_json::from_str(&c).unwrap();
        assert_eq!(v["result"].as_array().unwrap().len(), 8);
        // The folded answer's first hits are the expanded answer's first hits.
        let folded = json(&repo, &["refs", "impl_0"]);
        let first = &folded["folded"][0]["first"][0]["name"];
        let all = ok(&repo, &["--json", "refs", "impl_0", "--expand"]);
        let all: Value = serde_json::from_str(&all).unwrap();
        assert_eq!(&all["result"][0]["from"]["name"], first);
    }

    #[test]
    fn summary_line_format() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), 60, "");
        let t = text(&repo, &["refs", "impl_0"]);
        let fold = t.lines().next().unwrap();
        let want = "folded  calls  m:area  60 targets  7 refs  first ";
        assert!(fold.starts_with(want), "{fold}");
        assert!(
            fold.ends_with("  [Inferred]  (--expand lists all)"),
            "{fold}"
        );
        let first = &fold[want.len()..fold.len() - "  [Inferred]  (--expand lists all)".len()];
        let parts: Vec<&str> = first.split(", ").collect();
        assert_eq!(parts.len(), 5, "{fold}");
        assert!(parts.iter().all(|p| p.contains("caller_")), "{fold}");
        let c = text(&repo, &["callers", "impl_0"]);
        let fold = c.lines().find(|l| l.starts_with("folded")).unwrap();
        assert!(
            fold.starts_with("folded  calls  m:area  60 targets  7 callers  depth 1  first "),
            "{fold}"
        );
        assert!(
            fold.ends_with("  [Inferred]  (--expand lists all)"),
            "{fold}"
        );
    }

    #[test]
    fn envelope_additive() {
        let schema = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1");
        for entry in std::fs::read_dir(&schema).unwrap() {
            let p = entry.unwrap().path();
            let name = p.file_name().unwrap().to_str().unwrap().to_string();
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            let Some(props) = v.get("properties") else {
                continue;
            };
            if props.get("watermark").is_none() || props.get("result").is_none() {
                continue;
            }
            assert!(
                props.get("folded").is_some(),
                "{name} lacks an optional folded"
            );
            let required: Vec<&str> = v["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_str().unwrap())
                .collect();
            assert!(!required.contains(&"folded"), "{name}");
            assert_eq!(
                required,
                ["schema", "watermark", "pending", "stale", "result"],
                "{name}"
            );
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), 60, "");
        assert!(json(&repo, &["refs", "impl_0"]).get("folded").is_some());
        assert!(json(&repo, &["callers", "impl_0"]).get("folded").is_some());
        assert!(json(&repo, &["find", "impl_0"]).get("folded").is_none());
    }

    #[test]
    fn fold_is_not_partial() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), 60, "");
        for cmd in ["refs", "callers"] {
            let j = json(&repo, &[cmd, "impl_0"]);
            assert!(j.get("partial").is_none(), "{cmd}: {j}");
            assert!(j["folded"].is_array());
            let t = ok(&repo, &[cmd, "impl_0"]);
            assert!(!t.contains("partial:"), "{t}");
        }
    }

    #[test]
    fn depth_partial_same_when_folded() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), 60, "sym fn top\nlink top calls caller_0\n");
        let folded = json(&repo, &["callers", "impl_0", "--depth", "1"]);
        let expanded = json(&repo, &["callers", "impl_0", "--depth", "1", "--expand"]);
        assert_eq!(folded["partial"], expanded["partial"]);
        assert!(folded["partial"][0]["cap"].as_str().is_some(), "{folded}");
        assert_eq!(
            folded["partial"][0]["shown"],
            expanded["partial"][0]["shown"]
        );
        assert_eq!(folded["partial"][0]["shown"].as_u64(), Some(8));
    }

    #[test]
    fn budget_counts_fold_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), 60, "");
        let all = text(&repo, &["refs", "impl_0"]);
        // A budget that fits the fold line and not the line after it cuts there; the fold
        // line counts as a line.
        let fold_line = all.lines().next().unwrap();
        let direct = all.lines().nth(1).unwrap();
        let tokens = (fold_line.len() as u64).div_ceil(4) + 1;
        let (code, out, err) = run(&repo, &["refs", "impl_0", "--budget", &tokens.to_string()]);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains(fold_line), "{out}");
        assert!(!out.contains(direct), "{out}");
        assert!(out.contains("partial:"), "{out}");
    }

    #[test]
    fn diagnostic_names_folds() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), 60, "");
        let j = json(&repo, &["refs", "impl_0", "--diagnostic"]);
        let f = &j["diagnostic"]["folds"][0];
        assert_eq!(f["key"].as_str(), Some("m:area"));
        assert_eq!(
            (f["targets"].as_u64(), f["hits"].as_u64(), f["at"].as_u64()),
            (Some(60), Some(7), Some(0))
        );
        let x = json(&repo, &["refs", "impl_0", "--diagnostic", "--expand"]);
        assert!(x["diagnostic"].get("folds").is_none());
        assert_eq!(j["diagnostic"]["weights"], x["diagnostic"]["weights"]);
        let (_, _, err) = run(&repo, &["refs", "impl_0", "--diagnostic"]);
        assert!(
            err.contains("diagnostic: fold key=m:area kind=calls targets=60 hits=7 at=0"),
            "{err}"
        );
    }

    #[test]
    fn other_commands_never_fold() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), 60, "");
        for args in [
            vec!["map"],
            vec!["path", "caller_0", "impl_0"],
            vec!["explain", "impl_0"],
            vec!["tests", "impl_0"],
            vec!["search", "impl"],
            vec!["hubs"],
            vec!["cycles"],
            vec!["dead-code"],
            vec!["coverage"],
        ] {
            let (code, out, err) = run(&repo, &[&["--json"][..], &args[..]].concat());
            assert!(
                code == 0 || code == 1 || code == 4,
                "{args:?}: {code} {err}"
            );
            if let Ok(v) = serde_json::from_str::<Value>(&out) {
                assert!(v.get("folded").is_none(), "{args:?}");
            }
        }
        // The fan links are still there for the commands that show links.
        let e = ok(&repo, &["explain", "caller_0"]);
        assert!(e.contains("impl_0") && e.contains("of 60"), "{e}");
        let p = ok(&repo, &["path", "caller_0", "impl_0"]);
        assert!(p.contains("calls"), "{p}");
    }
}
