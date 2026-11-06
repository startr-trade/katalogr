//! `katalogr diff` end to end (FEAT-095), on a synthetic repo of `.tx` files.

mod diff_cli {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env_remove("KATALOGR_CONTEXT")
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

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    struct P {
        tmp: tempfile::TempDir,
        repo: PathBuf,
    }

    /// A project over a git repo with tags `v1` and `v2`, the base indexed at `v2`.
    fn project() -> P {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        ok(tmp.path(), &["kindex", "init", kx.to_str().unwrap()]);
        let spec = format!("app={}", repo.display());
        ok(
            tmp.path(),
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
        std::fs::write(repo.join("g.tx"), "sym fn a\nsym fn b\nlink b calls a\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c1"]);
        git(&repo, &["tag", "v1"]);
        std::fs::write(
            repo.join("g.tx"),
            "sym fn a\nsym fn b\nsym fn c\nsym fn d\nlink b calls a\nlink c calls a\nlink d calls c\n",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c2"]);
        git(&repo, &["tag", "v2"]);
        ok(
            &repo,
            &[
                "--timeout",
                "120",
                "--consistency",
                "wait-all",
                "index",
                "--full",
            ],
        );
        P { tmp, repo }
    }

    fn json(p: &P, args: &[&str]) -> serde_json::Value {
        let mut full = vec!["--json", "diff"];
        full.extend_from_slice(args);
        let out = ok(&p.repo, &full);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
    }

    // ---------------------------------------------------------------- R-1

    #[test]
    fn unknown_revision_exits_2() {
        let p = project();
        let (code, _, err) = run(&p.repo, &["diff", "v1", "no-such-rev"]);
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("no-such-rev"), "{err}");
    }

    #[test]
    fn repo_without_root_exits_2() {
        let p = project();
        let (code, _, err) = run(&p.repo, &["diff", "v1", "v2", "--repo", "ghost"]);
        assert_eq!(code, 2, "{err}");
    }

    // ---------------------------------------------------------------- R-7

    #[test]
    fn partial_shown_in_every_format() {
        let p = project();
        let v = json(&p, &["v1", "v2", "--limit", "1"]);
        assert!(
            v["partial"].as_array().is_some_and(|a| !a.is_empty()),
            "{v}"
        );
        let md = ok(&p.repo, &["diff", "v1", "v2", "--limit", "1"]);
        assert!(md.contains("capped at 1"), "{md}");
        let html = ok(
            &p.repo,
            &["diff", "v1", "v2", "--limit", "1", "--format", "html"],
        );
        assert!(html.contains("capped at 1"), "{html}");
    }

    // ---------------------------------------------------------------- R-12

    #[test]
    fn json_validates_against_schema() {
        let p = project();
        let v = json(&p, &["v1", "v2"]);
        let schema: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1/diff.schema.json"),
            )
            .unwrap(),
        )
        .unwrap();
        // The envelope's own fields, then the report's required fields.
        for key in schema["required"].as_array().unwrap() {
            assert!(v.get(key.as_str().unwrap()).is_some(), "{key} missing: {v}");
        }
        assert_eq!(v["schema"], 1);
        let report_def = &schema["$defs"]["DiffReport"];
        for key in report_def["required"].as_array().unwrap() {
            assert!(
                v["result"].get(key.as_str().unwrap()).is_some(),
                "{key} missing in the report"
            );
        }
        assert_eq!(v["result"]["report_format"], 1);
        assert_eq!(v["result"]["diff"]["symbols"]["added_total"], 2);
        // No absolute path anywhere.
        assert!(!v.to_string().contains(p.tmp.path().to_str().unwrap()));
    }

    // ---------------------------------------------------------------- R-15

    #[test]
    fn out_writes_one_format() {
        let p = project();
        let target = p.tmp.path().join("out/report.html");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        ok(
            &p.repo,
            &[
                "diff",
                "v1",
                "v2",
                "--format",
                "html",
                "--out",
                target.to_str().unwrap(),
            ],
        );
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(text.starts_with("<!DOCTYPE html>"));
        let names: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names.len(), 1, "no temp file is left: {names:?}");
    }

    #[test]
    fn out_dir_writes_all_formats() {
        let p = project();
        let dir = p.tmp.path().join("reports");
        ok(
            &p.repo,
            &["diff", "v1", "v2", "--out-dir", dir.to_str().unwrap()],
        );
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.iter().all(|n| n.starts_with("diff-") && n.len() > 30));
        for ext in [".html", ".json", ".md"] {
            assert!(names.iter().any(|n| n.ends_with(ext)), "{ext}: {names:?}");
        }
    }

    #[test]
    fn out_into_work_tree_refused() {
        let p = project();
        let target = p.repo.join("report.md");
        let (code, _, err) = run(
            &p.repo,
            &["diff", "v1", "v2", "--out", target.to_str().unwrap()],
        );
        assert_eq!(code, 2, "{err}");
        assert!(!target.exists());
        let (code, _, _) = run(
            &p.repo,
            &[
                "diff",
                "v1",
                "v2",
                "--out-dir",
                p.repo.join("docs").to_str().unwrap(),
            ],
        );
        assert_eq!(code, 2);
    }

    // ---------------------------------------------------------------- R-19

    #[test]
    fn works_without_board_or_server() {
        let p = project();
        // No board, no server: only the index and the repo's git objects.
        let out = ok(&p.repo, &["diff", "v1", "v2"]);
        assert!(out.starts_with("# Changes in app"), "{out}");
        assert!(out.contains("app:g.tx#fn:d"), "{out}");
    }
}
