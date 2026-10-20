//! Partial markers on capped and budgeted answers (FEAT-062, ADR-0025).

mod partial {
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

    /// The parsed `--json` answer.
    fn json(repo: &Path, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let (_, out, err) = run(repo, &all);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{all:?}: {e}: {out}{err}"))
    }

    /// A project `demo` with one marked repo `app` holding `files`, indexed.
    fn project(tmp: &Path, files: &[(&str, String)]) -> PathBuf {
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
        for (name, text) in files {
            std::fs::write(repo.join(name), text).unwrap();
        }
        ok(&repo, &["index"]);
        repo
    }

    const NOTE: &str = "absence is not proof";

    fn render_corpus() -> Vec<(&'static str, String)> {
        let names = ["alpha", "beta", "gamma", "delta", "epsilon"];
        let text: String = names
            .iter()
            .map(|n| format!("sym fn render_{n}\n"))
            .collect();
        vec![("r.tx", text)]
    }

    fn chain(len: usize) -> Vec<(&'static str, String)> {
        let mut text = String::new();
        for i in 0..len {
            text.push_str(&format!("sym fn chain_{i}\n"));
        }
        for i in 1..len {
            text.push_str(&format!("link chain_{i} calls chain_{}\n", i - 1));
        }
        vec![("c.tx", text)]
    }

    #[test]
    fn search_limit_marked() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), &render_corpus());
        let v = json(&repo, &["search", "render", "--limit", "3"]);
        assert_eq!(v["result"].as_array().unwrap().len(), 3, "{v}");
        let p = &v["partial"][0];
        assert_eq!(p["cap"], "limit", "{v}");
        assert_eq!(p["at"], 3);
        assert_eq!(p["shown"], 3);
        assert_eq!(p["note"], NOTE);
        assert!(p.get("total").is_none(), "{v}");
        assert_eq!(v["partial"].as_array().unwrap().len(), 1);
        let text = ok(&repo, &["search", "render", "--limit", "3"]);
        assert!(
            text.contains(&format!("partial: capped at 3 (limit); {NOTE}")),
            "{text}"
        );
    }

    #[test]
    fn callers_depth_marked() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), &chain(4));
        let v = json(&repo, &["callers", "chain_0", "--depth", "2"]);
        assert_eq!(v["result"].as_array().unwrap().len(), 2, "{v}");
        let p = &v["partial"][0];
        assert_eq!(p["cap"], "depth", "{v}");
        assert_eq!(p["at"], 2);
        assert_eq!(p["shown"], 2);
        let text = ok(&repo, &["callers", "chain_0", "--depth", "2"]);
        assert!(
            text.contains(&format!("partial: capped at 2 (depth); {NOTE}")),
            "{text}"
        );
    }

    #[test]
    fn explain_per_side_marked() {
        let tmp = tempfile::tempdir().unwrap();
        let mut text = String::from("sym fn busy\n");
        for i in 0..4 {
            text.push_str(&format!("sym fn leaf_{i}\nlink busy calls leaf_{i}\n"));
        }
        let repo = project(tmp.path(), &[("e.tx", text)]);
        let v = json(&repo, &["explain", "busy", "--per-side", "2"]);
        let p = &v["partial"][0];
        assert_eq!(p["cap"], "per-side", "{v}");
        assert_eq!(p["at"], 2);
        assert_eq!(p["shown"], 2);
        assert_eq!(p["total"], 4);
        let text = ok(&repo, &["explain", "busy", "--per-side", "2"]);
        assert!(
            text.contains(&format!("partial: capped at 2 (per-side; 4 found); {NOTE}")),
            "{text}"
        );
    }

    #[test]
    fn analytics_limit_marked() {
        let tmp = tempfile::tempdir().unwrap();
        let mut text = String::new();
        for h in 0..3 {
            text.push_str(&format!("sym fn hub_{h}\n"));
            for c in 0..3 {
                text.push_str(&format!(
                    "sym fn user_{h}_{c}\nlink user_{h}_{c} calls hub_{h}\n"
                ));
            }
        }
        let repo = project(tmp.path(), &[("h.tx", text)]);
        let v = json(&repo, &["hubs", "--min-degree", "1", "--limit", "1"]);
        assert_eq!(v["result"]["items"].as_array().unwrap().len(), 1, "{v}");
        let p = &v["partial"][0];
        assert_eq!(p["cap"], "limit", "{v}");
        assert_eq!(p["at"], 1);
        let text = ok(&repo, &["hubs", "--min-degree", "1", "--limit", "1"]);
        assert!(
            text.contains(&format!("partial: capped at 1 (limit); {NOTE}")),
            "{text}"
        );
    }

    #[test]
    fn budget_marked() {
        let tmp = tempfile::tempdir().unwrap();
        let mut text = String::from("sym fn target\n");
        for i in 0..8 {
            text.push_str(&format!(
                "sym fn caller_{i}\nlink caller_{i} calls target\n"
            ));
        }
        let repo = project(tmp.path(), &[("b.tx", text)]);
        let v = json(&repo, &["refs", "target", "--budget", "20"]);
        assert!(v["truncated"].is_object(), "{v}");
        let p = v["partial"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["cap"] == "budget")
            .unwrap_or_else(|| panic!("no budget entry: {v}"));
        assert_eq!(p["at"], 20);
        assert_eq!(p["shown"], v["truncated"]["shown"]);
        assert_eq!(p["total"], 8);
        let text = ok(&repo, &["refs", "target", "--budget", "20"]);
        assert!(
            text.contains("partial: capped at 20 tokens (budget; 8 found)"),
            "{text}"
        );
    }

    #[test]
    fn session_start_marked() {
        let tmp = tempfile::tempdir().unwrap();
        let text: String = (0..30)
            .map(|i| format!("sym fn symbol_number_{i}\n"))
            .collect();
        let repo = project(tmp.path(), &[("s.tx", text)]);
        let out = ok(&repo, &["session-start", "--budget", "120"]);
        assert!(
            out.contains(
                "partial: capped at 120 tokens (budget; 30 symbols); absence is not proof"
            ),
            "{out}"
        );
        assert!((out.chars().count() as u64).div_ceil(4) <= 120, "{out}");
        // Without a cut there is no marker.
        let out = ok(&repo, &["session-start", "--budget", "4000"]);
        assert!(!out.contains("partial:"), "{out}");
    }

    #[test]
    fn uncapped_has_no_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let mut files = render_corpus();
        files.extend(chain(3));
        let mut e = String::from("sym fn busy\n");
        for i in 0..2 {
            e.push_str(&format!("sym fn leaf_{i}\nlink busy calls leaf_{i}\n"));
        }
        files.push(("e.tx", e));
        let repo = project(tmp.path(), &files);
        for args in [
            vec!["search", "render", "--limit", "10"],
            vec!["callers", "chain_0", "--depth", "3"],
            vec!["explain", "busy", "--per-side", "5"],
        ] {
            let v = json(&repo, &args);
            assert!(v.get("partial").is_none(), "{args:?}: {v}");
            let text = ok(&repo, &args);
            assert!(!text.contains("partial:"), "{args:?}: {text}");
        }
    }

    #[test]
    fn path_miss_says_not_proof() {
        let tmp = tempfile::tempdir().unwrap();
        let text = "sym fn lone_one\nsym fn lone_two\n".to_string();
        let repo = project(tmp.path(), &[("p.tx", text)]);
        let (code, _, err) = run(&repo, &["path", "lone_one", "lone_two", "--max-len", "2"]);
        assert_eq!(code, 4, "{err}");
        assert!(err.contains(NOTE), "{err}");
        assert!(err.contains("searched up to 2 links"), "{err}");
    }

    #[test]
    fn envelope_additive() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let file = entry.unwrap().path();
            if !file.to_string_lossy().ends_with(".schema.json") {
                continue;
            }
            // `doctor` is its own report type, not an envelope.
            if file.file_name().is_some_and(|n| n == "doctor.schema.json") {
                continue;
            }
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            let name = file.display();
            assert!(v["properties"]["partial"].is_object(), "{name}: no partial");
            let required: Vec<&str> = v["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_str().unwrap())
                .collect();
            assert_eq!(
                required,
                ["schema", "watermark", "pending", "stale", "result"],
                "{name}"
            );
            seen += 1;
        }
        assert!(seen >= 16, "only {seen} schemas");
    }
}
