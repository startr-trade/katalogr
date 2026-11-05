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
        // The checkout's own context sees the disk; the base reflects the commit.
        let (code, out, _) = run(&repo, &["find", "Dirty"]);
        assert_eq!(code, 0, "{out}");
        let mut k = katalogr_dist::Katalogr::resolve(&repo, &Default::default()).unwrap();
        k.set_context(katalogr_dist::contexts::ContextChoice::Base);
        let found = |k: &katalogr_dist::Katalogr, t: &str| {
            format!("{:?}", k.find(t, &Default::default()).unwrap().outcome).starts_with("Found")
        };
        assert!(found(&k, "Committed"));
        assert!(!found(&k, "Dirty"));
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

    #[test]
    fn context_flag_selects_base() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), &[("a.tx", "sym fn Committed\n")]);
        std::fs::write(repo.join("a.tx"), "sym fn Dirty\n").unwrap();
        ok(&repo, &["index"]);
        // The default is the checkout's own context.
        assert!(ok(&repo, &["find", "Dirty"]).contains("Dirty"));
        assert!(ok(&repo, &["--context", "current", "find", "Dirty"]).contains("Dirty"));
        // The base holds the commit.
        let (code, out, _) = run(&repo, &["--context", "base", "find", "Dirty"]);
        assert_eq!(code, 4, "{out}");
        assert!(ok(&repo, &["--context", "base", "find", "Committed"]).contains("Committed"));
        // The environment variable does the same, and the flag wins over it.
        let by_env = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(&repo)
            .env("KATALOGR_CONTEXT", "base")
            .args(["find", "Dirty"])
            .output()
            .unwrap();
        assert_eq!(by_env.status.code(), Some(4));
        let flag_wins = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(&repo)
            .env("KATALOGR_CONTEXT", "base")
            .args(["--context", "current", "find", "Dirty"])
            .output()
            .unwrap();
        assert_eq!(flag_wins.status.code(), Some(0));
    }

    #[test]
    fn unknown_context_exit_2() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), &[("a.tx", "sym fn A\n")]);
        for bad in ["w-0123456789abcdef", "Not Valid", "a/b"] {
            let (code, out, err) = run(&repo, &["--context", bad, "find", "A"]);
            assert_eq!(code, 2, "{bad}: {out}{err}");
        }
    }
}
