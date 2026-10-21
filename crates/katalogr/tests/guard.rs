//! The privacy guard through the CLI (FEAT-073): a relative temp directory and a refused file
//! never take down the other commands.

mod guard {
    use std::path::Path;
    use std::process::Command;

    fn run(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_katalogr"));
        c.current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args);
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.output().expect("run katalogr");
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).expect("utf8"),
            String::from_utf8(out.stderr).expect("utf8"),
        )
    }

    fn ok(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> String {
        let (code, out, err) = run(dir, env, args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        out
    }

    fn git(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.test"])
            .args(args)
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?}");
    }

    /// A git repository `app` with one commit holding `files`, and a marked project over it.
    fn project(tmp: &Path, files: &[(&str, &str)]) -> std::path::PathBuf {
        let repo = tmp.join("app");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        for (p, text) in files {
            std::fs::write(repo.join(p), text).unwrap();
        }
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        let kx = tmp.join("kx");
        ok(tmp, &[], &["kindex", "init", kx.to_str().unwrap()]);
        ok(
            tmp,
            &[],
            &[
                "--kindex",
                kx.to_str().unwrap(),
                "project",
                "init",
                "demo",
                "--repo",
                &format!("app={}", repo.display()),
                "--mark",
            ],
        );
        repo
    }

    #[test]
    fn queries_survive_relative_tmpdir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[(
                "src/a.ts",
                "export function tmpDir(portableDataPath: string) { return path.join(portableDataPath, \"tmp\"); }\n",
            )],
        );
        let env = [("TMPDIR", "tmp")];
        ok(&repo, &env, &["index", "--full"]);
        for cmd in ["index", "hubs", "dead-code", "coverage"] {
            ok(&repo, &env, &[cmd]);
        }
        let map = ok(&repo, &env, &["map"]);
        assert!(map.contains("fn:tmpDir"), "{map}");
    }

    #[test]
    fn refused_file_does_not_abort_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                (
                    "src/bad.ts",
                    "export function bad() { return fs.readFileSync(\"/home/synthuser/data.txt\", \"utf8\"); }\n",
                ),
                ("src/good.ts", "export function good() { return 1; }\n"),
            ],
        );
        let env = [("HOME", "/home/synthuser")];
        ok(&repo, &env, &["index", "--full"]);
        ok(&repo, &env, &["index"]);
        let map = ok(&repo, &env, &["map"]);
        assert!(map.contains("fn:good"), "{map}");
        assert!(!map.contains("fn:bad"), "{map}");
        let (code, cov, err) = run(&repo, &env, &["--json", "coverage"]);
        assert_eq!(code, 0, "{cov}{err}");
        let v: serde_json::Value = serde_json::from_str(&cov).unwrap();
        assert_eq!(v["result"]["totals"]["files_failed"], 1, "{cov}");
        assert!(!cov.contains("/home/synthuser"), "{cov}");
        assert!(!err.contains("/home/synthuser"), "{err}");
    }
}
