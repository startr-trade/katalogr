//! Contexts through the command line (FEAT-043).

mod cli {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
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

    pub fn ok(dir: &Path, args: &[&str]) -> String {
        let (code, out, err) = run(dir, args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        out
    }

    pub fn git(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?}");
    }

    /// A project `demo` with a marked git repo `app` on `main`, holding `files` committed.
    pub fn project(tmp: &Path, files: &[(&str, &str)]) -> PathBuf {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
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
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c1"]);
        repo
    }

    #[test]
    fn base_reflects_commit_not_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), &[("a.tx", "sym fn Committed\n")]);
        std::fs::write(repo.join("a.tx"), "sym fn Dirty\n").unwrap();
        ok(&repo, &["index"]);
        let base = ok(&repo, &["find", "Committed"]);
        assert!(base.contains("Committed"), "{base}");
        let (code, out, _) = run(&repo, &["find", "Dirty"]);
        assert!(code != 0 && !out.contains("Dirty"), "{out}");
        // The recorded commit is the default branch's tip.
        let k = katalogr_dist::Katalogr::resolve(&repo, &Default::default()).unwrap();
        let head = Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let head = String::from_utf8(head.stdout).unwrap().trim().to_string();
        assert_eq!(
            k.base_commit("app", &Default::default())
                .unwrap()
                .as_deref(),
            Some(head.as_str())
        );
    }
}
