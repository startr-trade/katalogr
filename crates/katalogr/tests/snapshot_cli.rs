//! `katalogr snapshot` through the real binary (FEAT-097 R-15, R-16).

mod snapshot_cli {
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
        let st = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?}");
    }

    /// An indexed project `demo` with a marked, committed repo `app`, and a snapshot of it at
    /// `tmp/g.ksnap` (outside the work tree). Returns the repo and the snapshot.
    fn project(tmp: &Path) -> (PathBuf, PathBuf) {
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
        std::fs::write(repo.join("a.tx"), "sym fn alpha\nsym fn beta\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c1"]);
        ok(&repo, &["index"]);
        let snap = tmp.join("g.ksnap");
        ok(
            &repo,
            &["snapshot", "export", "--out", snap.to_str().unwrap()],
        );
        (repo, snap)
    }

    #[test]
    fn show_prints_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, snap) = project(tmp.path());
        // Needs no project: run from a folder that is in none.
        let elsewhere = tempfile::tempdir().unwrap();
        let text = ok(
            elsewhere.path(),
            &["snapshot", "show", snap.to_str().unwrap()],
        );
        assert!(text.contains("format: 1"), "{text}");
        assert!(text.contains("position app: "), "{text}");
        assert!(text.contains("digest: "), "{text}");
        let json = ok(
            elsewhere.path(),
            &["--json", "snapshot", "show", snap.to_str().unwrap()],
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["format"], 1);
        assert_eq!(v["positions"][0]["repo"], "app");
        assert_eq!(v["digest"].as_str().unwrap().len(), 64);
        let _ = repo;
    }

    #[test]
    fn show_verify_detects_damage() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, snap) = project(tmp.path());
        let text = ok(
            &repo,
            &["snapshot", "show", snap.to_str().unwrap(), "--verify"],
        );
        assert!(text.contains("payload verified"), "{text}");
        let mut raw = std::fs::read(&snap).unwrap();
        let mid = raw.len() / 2;
        raw[mid] ^= 0xff;
        std::fs::write(&snap, raw).unwrap();
        // The header alone still reads; the payload does not verify.
        ok(&repo, &["snapshot", "show", snap.to_str().unwrap()]);
        let (code, _, err) = run(
            &repo,
            &["snapshot", "show", snap.to_str().unwrap(), "--verify"],
        );
        assert_eq!(code, 1, "{err}");
        // No verify copy is left in the cache folder.
        let cache = tmp.path().join("kx/projects/demo/.cache");
        assert!(!cache.join("snapshot-verify.tmp").exists());
    }

    #[test]
    fn import_digest_mismatch_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, snap) = project(tmp.path());
        let wrong = "0".repeat(64);
        let (code, _, err) = run(
            &repo,
            &[
                "snapshot",
                "import",
                snap.to_str().unwrap(),
                "--replace",
                "--digest",
                &wrong,
            ],
        );
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("digest"), "{err}");
        // --context is a usage error, whatever else is right.
        let (code, _, err) = run(
            &repo,
            &[
                "--context",
                "base",
                "snapshot",
                "import",
                snap.to_str().unwrap(),
                "--replace",
            ],
        );
        assert_eq!(code, 2, "{err}");
        // The right digest installs.
        let shown = ok(
            &repo,
            &["--json", "snapshot", "show", snap.to_str().unwrap()],
        );
        let v: serde_json::Value = serde_json::from_str(&shown).unwrap();
        let digest = v["digest"].as_str().unwrap();
        let out = ok(
            &repo,
            &[
                "snapshot",
                "import",
                snap.to_str().unwrap(),
                "--replace",
                "--digest",
                digest,
            ],
        );
        assert!(out.contains("installed"), "{out}");
    }
}
