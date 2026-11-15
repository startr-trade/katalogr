//! FEAT-085: a linked worktree through the command line (R-4 by environment, R-7).

mod worktree {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn run(dir: &Path, env: Option<&str>, args: &[&str]) -> (i32, String, String) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_katalogr"));
        c.current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env_remove("KATALOGR_CONTEXT")
            .args(args);
        if let Some(e) = env {
            c.env("KATALOGR_CONTEXT", e);
        }
        let out = c.output().expect("run katalogr");
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    fn git(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                // Git for Windows defaults to core.autocrlf=true: a checkout would rewrite the
                // fixtures with CRLF, and the position (raw bytes) would read them as edited.
                "-c",
                "core.autocrlf=false",
            ])
            .args(args)
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?}");
    }

    /// A project with a marked git repo (marker untracked) and a linked worktree holding an
    /// extra file `b.tx`. Returns (main, worktree).
    fn world(tmp: &Path) -> (PathBuf, PathBuf) {
        // Without Windows' verbatim prefix: git cannot create a worktree at `\\?\C:\...`.
        let tmp = katalogr_store::canon::canonicalize(tmp).unwrap();
        let kx = tmp.join("kx");
        let main = tmp.join("app");
        let wt = tmp.join("app-wt");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        std::fs::write(main.join("a.tx"), "sym fn basefn\n").unwrap();
        std::fs::write(main.join(".gitignore"), ".katalogr\n").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-q", "-m", "base"]);
        git(
            &main,
            &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
        );
        let (c, o, e) = run(&tmp, None, &["kindex", "init", kx.to_str().unwrap()]);
        assert_eq!(c, 0, "{o}{e}");
        let spec = format!("app={}", main.display());
        let (c, o, e) = run(
            &tmp,
            None,
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
        assert_eq!(c, 0, "{o}{e}");
        std::fs::write(wt.join("b.tx"), "sym fn wtonlyfn\n").unwrap();
        (main, wt)
    }

    #[test]
    fn context_env_reads_another_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, wt) = world(tmp.path());
        assert_eq!(run(&wt, None, &["index"]).0, 0);
        let (_, out, _) = run(&wt, None, &["--json", "status"]);
        let id = serde_json::from_str::<serde_json::Value>(&out).unwrap()["context"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(run(&main, Some(&id), &["find", "wtonlyfn"]).0, 0);
        assert_eq!(run(&main, None, &["find", "wtonlyfn"]).0, 4);
    }

    #[test]
    fn status_names_context() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, wt) = world(tmp.path());
        let ctx = |dir: &Path| {
            let (c, out, err) = run(dir, None, &["--json", "status"]);
            assert_eq!(c, 0, "{out}{err}");
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert!(v["watermark"].is_u64() && v["pending"].is_u64());
            let id = v["context"].as_str().expect("context").to_string();
            assert!(!out.contains(dir.to_str().unwrap()), "no path in {out}");
            id
        };
        let (a, b) = (ctx(&main), ctx(&wt));
        assert!(a.starts_with("w-") && b.starts_with("w-"), "{a} {b}");
        assert_ne!(a, b);
        let (_, text, _) = run(&wt, None, &["status"]);
        assert!(text.contains(&format!("context {b}")), "{text}");
        let (_, out, _) = run(&wt, None, &["--context", "base", "--json", "status"]);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap()["context"],
            "base"
        );
    }
}
