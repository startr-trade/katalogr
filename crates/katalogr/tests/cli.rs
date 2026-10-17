//! CLI-level tests: run the built binary.

mod cli {
    use std::process::Command;

    fn version_output() -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .arg("--version")
            .output()
            .expect("run katalogr --version");
        assert!(
            out.status.success(),
            "--version exited with {:?}",
            out.status
        );
        String::from_utf8(out.stdout).expect("utf8 --version output")
    }

    /// Extracts `<sha>` from `katalogr <semver> (<sha> <date>)`.
    fn build_sha(line: &str) -> &str {
        let inner = line
            .split_once('(')
            .and_then(|(_, rest)| rest.strip_suffix(")\n").or_else(|| rest.strip_suffix(')')))
            .unwrap_or_else(|| panic!("unexpected version format: {line:?}"));
        inner
            .split_once(' ')
            .map(|(sha, _date)| sha)
            .unwrap_or_else(|| panic!("no date in version: {line:?}"))
    }

    use std::path::{Path, PathBuf};

    pub fn katalogr(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .output()
            .expect("run katalogr")
    }

    pub fn ok(dir: &Path, args: &[&str]) -> String {
        let out = katalogr(dir, args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8 stdout")
    }

    /// A kindex and a project `demo` with one marked repo `app` holding a file.
    pub fn setup(tmp: &Path) -> (PathBuf, PathBuf) {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
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
        (kx, repo)
    }

    #[test]
    fn kindex_and_project_init() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = setup(tmp.path());
        let project = kx.join("projects/demo");
        for sub in ["profiles", "shards", ".cache"] {
            assert!(project.join(sub).is_dir(), "missing {sub}");
        }
        assert!(kx.join(".git").exists());
        assert!(project.join("format.json").is_file());
        let config = std::fs::read_to_string(project.join("config.yaml")).unwrap();
        assert!(config.contains("name: app"), "{config}");
        assert!(
            !config.contains(repo.to_str().unwrap()),
            "root leaked into the kindex: {config}"
        );

        let roots = std::fs::read_to_string(project.join(".cache/roots.yaml")).unwrap();
        let canon = std::fs::canonicalize(&repo).unwrap();
        assert_eq!(roots, format!("app: {}\n", canon.display()));

        let marker = std::fs::read_to_string(repo.join(".katalogr")).unwrap();
        assert_eq!(marker, "kindex: ../kx\nproject: demo\nrepo: app\n");

        // The marker alone resolves everything: add-root works with no flags.
        ok(
            &repo,
            &["project", "add-root", "app", repo.to_str().unwrap()],
        );
    }

    #[test]
    fn notify_then_index() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = setup(tmp.path());
        let out = ok(&repo, &["notify", "a.txt"]);
        assert!(out.contains("queued 1"), "{out}");
        let json = ok(&repo, &["--json", "index"]);
        let v: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(v["pending"], 0, "{json}");
        assert_eq!(v["stale"], false, "{json}");
        assert!(v["watermark"].as_u64().unwrap() >= 1, "{json}");
    }

    #[test]
    fn version_names_commit() {
        let line = version_output();
        assert!(
            line.contains(env!("CARGO_PKG_VERSION")),
            "missing semver in {line:?}"
        );

        let sha = build_sha(&line);
        let is_12_hex = sha.len() == 12 && sha.bytes().all(|b| b.is_ascii_hexdigit());
        assert!(
            is_12_hex || sha == "unknown",
            "sha must be 12 hex chars or `unknown`, got {sha:?} in {line:?}"
        );

        if let Some(expected) = option_env!("KATALOGR_GIT_SHA") {
            assert!(
                line.contains(expected),
                "KATALOGR_GIT_SHA={expected} at build time, but output is {line:?}"
            );
        }
    }
}

mod query {
    use super::cli::{katalogr, setup};
    use fs4::FileExt;

    #[test]
    fn timeout_returns_stale_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = setup(tmp.path());
        // Hold the project's processor lock from this process; the CLI cannot take it.
        let lock = kx.join("projects/demo/.cache/processor.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock)
            .unwrap();
        FileExt::try_lock(&file).expect("take the processor lock");

        let out = katalogr(&repo, &["--json", "index", "--timeout", "0.2"]);
        assert_eq!(out.status.code(), Some(5), "{out:?}");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
        assert_eq!(v["stale"], true);
        assert_eq!(v["pending"], 1);
    }
}
