//! FEAT-096: stored answers through the real binary. The project is a committed git repo of
//! `.tx` files (a stored answer needs a clean, exact position), and `KATALOGR_ANSWER_BUILD`
//! names the build, since a test binary has no clean commit of its own.

mod answers_cli {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    const BUILD: &str = "test-build";

    fn git(root: &Path, args: &[&str]) {
        let st = Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?} failed");
    }

    fn run_env(dir: &Path, build: Option<&str>, args: &[&str]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_katalogr"));
        c.current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env_remove("KANBANR_DATA_DIR")
            .args(args);
        match build {
            Some(b) => c.env("KATALOGR_ANSWER_BUILD", b),
            None => c.env_remove("KATALOGR_ANSWER_BUILD"),
        };
        c.output().expect("run katalogr")
    }

    fn run(dir: &Path, args: &[&str]) -> (i32, String) {
        let out = run_env(dir, Some(BUILD), args);
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).unwrap(),
        )
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let (code, out) = run(dir, args);
        assert_eq!(code, 0, "{args:?}: {out}");
        out
    }

    /// A kindex and a project over a committed repo, indexed. Returns the kindex and repo.
    pub fn project(tmp: &Path) -> (PathBuf, PathBuf) {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("g.tx"),
            "sym fn hub\nsym fn a\nsym fn b\nsym fn c\nsym fn lonely\n\
             link a calls hub\nlink b calls hub\nlink c calls hub\nlink a calls b\nlink b calls a\n",
        )
        .unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "c1"]);
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
        ok(&repo, &["index"]);
        (kx, repo)
    }

    fn stats(repo: &Path) -> serde_json::Value {
        serde_json::from_str(&ok(repo, &["answers", "stats", "--json"])).unwrap()
    }

    const FIRST_FIVE: [&[&str]; 6] = [
        &["hubs", "--min-degree", "0"],
        &["communities"],
        &["cycles"],
        &["dead-code"],
        &["map", "--neutral"],
        &["coverage"],
    ];

    #[test]
    fn hit_matches_fresh_json() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        for cmd in FIRST_FIVE {
            let mut args = cmd.to_vec();
            args.push("--json");
            let first = ok(&repo, &args);
            let hit = ok(&repo, &args);
            let mut fresh_args = vec!["--fresh"];
            fresh_args.extend(&args);
            let fresh = ok(&repo, &fresh_args);
            assert_eq!(first, hit, "{cmd:?}: a hit is the first answer");
            assert_eq!(fresh, hit, "{cmd:?}: and the fresh one");
        }
        assert_eq!(stats(&repo)["entries"], 6);
    }

    #[test]
    fn hit_matches_fresh_text() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        for cmd in FIRST_FIVE {
            let first = ok(&repo, cmd);
            let hit = ok(&repo, cmd);
            let mut fresh_args = vec!["--fresh"];
            fresh_args.extend(cmd);
            let fresh = ok(&repo, &fresh_args);
            assert_eq!(first, hit, "{cmd:?}");
            assert_eq!(fresh, hit, "{cmd:?}");
        }
    }

    #[test]
    fn budget_uses_same_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let full = ok(&repo, &["hubs", "--min-degree", "0", "--json"]);
        let cut = ok(
            &repo,
            &["hubs", "--min-degree", "0", "--budget", "40", "--json"],
        );
        assert_ne!(full, cut, "the budget cuts what is shown");
        assert_eq!(stats(&repo)["entries"], 1, "one entry served both");
        // The cut answer from the entry equals the cut answer computed fresh.
        let fresh = ok(
            &repo,
            &[
                "--fresh",
                "hubs",
                "--min-degree",
                "0",
                "--budget",
                "40",
                "--json",
            ],
        );
        assert_eq!(cut, fresh);
    }

    #[test]
    fn check_exit_codes() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = project(tmp.path());
        ok(&repo, &["hubs", "--min-degree", "0"]);
        ok(&repo, &["cycles"]);
        let (code, out) = run(&repo, &["answers", "check", "--json"]);
        assert_eq!(code, 0, "{out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            (v["checked"].as_u64(), v["matched"].as_u64()),
            (Some(2), Some(2))
        );
        // Change one digit of an entry, keeping its length.
        let dir = kx.join("projects/demo/.cache/answers");
        let file = std::fs::read_dir(&dir)
            .unwrap()
            .flat_map(|d| std::fs::read_dir(d.unwrap().path()).unwrap())
            .map(|f| f.unwrap().path())
            .find(|p| {
                std::fs::read_to_string(p)
                    .unwrap()
                    .contains("\"op\":\"hubs\"")
            })
            .unwrap();
        let mut bytes = std::fs::read(&file).unwrap();
        let at = bytes.iter().position(|b| *b == b'\n').unwrap() + 1;
        let d = at
            + bytes[at..]
                .iter()
                .position(|b| b.is_ascii_digit() && *b != b'9')
                .unwrap();
        bytes[d] += 1;
        std::fs::write(&file, bytes).unwrap();
        let (code, out) = run(&repo, &["answers", "check"]);
        assert_eq!(code, 1, "{out}");
        let (code, _) = run(&repo, &["answers", "check", "--repair"]);
        assert_eq!(code, 1, "a repair still reports what it found");
        let (code, out) = run(&repo, &["answers", "check"]);
        assert_eq!(code, 0, "{out}");
        // A read that cannot hold the position is not exact: 5.
        let (code, _) = run(&repo, &["--consistency", "stale-ok", "answers", "check"]);
        assert_eq!(code, 5);
    }

    #[test]
    fn stats_and_prune() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        for cmd in FIRST_FIVE {
            ok(&repo, cmd);
        }
        let s = stats(&repo);
        assert_eq!(s["entries"], 6);
        assert!(s["bytes"].as_u64().unwrap() > 0);
        assert_eq!(s["max_mib"], 256);
        assert_eq!(s["by_op"]["hubs"], 1);
        let text = ok(&repo, &["answers", "stats"]);
        assert!(text.starts_with("6 stored answers"), "{text}");
        let pruned: serde_json::Value = serde_json::from_str(&ok(
            &repo,
            &["answers", "prune", "--max-mib", "0", "--json"],
        ))
        .unwrap();
        assert_eq!(pruned["removed"], 6);
        assert_eq!(stats(&repo)["entries"], 0);
    }

    #[test]
    fn commands_work_without_board() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        // No board and no agent anywhere: the PATH holds only the system tools.
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(&repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", tmp.path())
            .env("KATALOGR_ANSWER_BUILD", BUILD)
            .args(["hubs", "--min-degree", "0", "--json"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(&repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", tmp.path())
            .env("KATALOGR_ANSWER_BUILD", BUILD)
            .args(["answers", "stats", "--json"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["entries"], 1);
        // Without a build, nothing is stored.
        let none = run_env(&repo, None, &["answers", "stats", "--json"]);
        assert_eq!(none.status.code(), Some(0));
    }
}
