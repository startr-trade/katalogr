//! FEAT-064 T9: the `katalogr doctor` command, run as a process.

use std::path::{Path, PathBuf};
use std::process::Command;

fn katalogr(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_katalogr"))
        .current_dir(dir)
        .env_remove("KATALOGR_KINDEX")
        .env_remove("KATALOGR_PROJECT")
        .args(args)
        .output()
        .expect("run katalogr")
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let out = katalogr(dir, args);
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A kindex and project `demo` with three `.tx` files indexed and the kindex committed.
/// Returns the kindex and repo directories.
fn healthy(tmp: &Path) -> (PathBuf, PathBuf) {
    let kx = tmp.join("kx");
    let repo = tmp.join("app");
    std::fs::create_dir_all(&repo).unwrap();
    for (n, t) in [
        ("a.tx", "sym fn a\n"),
        ("b.tx", "sym fn b\n"),
        ("c.tx", "sym fn c\n"),
    ] {
        std::fs::write(repo.join(n), t).unwrap();
    }
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
    git(&kx, &["add", "-A"]);
    git(&kx, &["commit", "-m", "index"]);
    (kx, repo)
}

/// Edits one shard after the commit and indexing: two warnings (cache and git).
fn dirty(kx: &Path) {
    let shard = kx.join("projects/demo/shards/app/a.tx.kshard.json");
    let mut bytes = std::fs::read(&shard).unwrap();
    bytes.push(b' ');
    std::fs::write(shard, bytes).unwrap();
}

mod doctor_cli {
    use super::*;

    #[test]
    fn exit_codes() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = healthy(tmp.path());
        let out = katalogr(&repo, &["doctor"]);
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(text.contains("summary: healthy; 11 checks"), "{text}");
        assert!(text.starts_with("katalogr doctor: katalogr "), "{text}");
        assert!(
            text.contains("shard format 1") && text.contains("cache schema "),
            "{text}"
        );

        // A warning-only project: exit 1, but 0 when only errors count.
        dirty(&kx);
        assert_eq!(katalogr(&repo, &["doctor"]).status.code(), Some(1));
        assert_eq!(
            katalogr(&repo, &["doctor", "--fail-on", "warning"])
                .status
                .code(),
            Some(1)
        );
        assert_eq!(
            katalogr(&repo, &["doctor", "--fail-on", "error"])
                .status
                .code(),
            Some(0)
        );
        // --skip-git leaves the cache warning.
        assert_eq!(
            katalogr(&repo, &["doctor", "--skip-git"]).status.code(),
            Some(1)
        );

        // An error: format.json is broken.
        std::fs::write(kx.join("projects/demo/format.json"), "{").unwrap();
        let out = katalogr(&repo, &["doctor", "--fail-on", "error"]);
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stdout).contains("KD-FMT-001"));

        // No project at all is a finding, not a usage error.
        let bare = tempfile::tempdir().unwrap();
        let out = katalogr(bare.path(), &["doctor"]);
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stdout).contains("KD-MRK-001"));
    }

    #[test]
    fn text_and_json_agree() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = healthy(tmp.path());
        dirty(&kx);
        std::fs::write(kx.join("projects/demo/shards/app/.tmp-x.kshard.json"), "{}").unwrap();
        let text = ok_or_fail(&repo, &["doctor", "--verbose"]);
        let json = ok_or_fail(&repo, &["--json", "doctor", "--verbose"]);
        assert_eq!(json.lines().count(), 1, "one JSON object on one line");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema"], 1);
        assert_eq!(v["healthy"], false);
        let findings = v["findings"].as_array().unwrap();
        assert!(findings.len() >= 3, "{json}");
        let mut last = 0;
        for f in findings {
            let code = f["code"].as_str().unwrap();
            let sev = f["severity"].as_str().unwrap();
            let line = format!("{sev:<7} {code}  {}", f["message"].as_str().unwrap());
            let at = text
                .find(&line)
                .unwrap_or_else(|| panic!("{line:?} not in\n{text}"));
            assert!(at >= last, "same order");
            last = at;
            if let Some(fix) = f["fix"].as_str() {
                assert!(text.contains(&format!("fix: {fix}")), "{fix}");
            }
            for k in f["sample"].as_array().unwrap() {
                assert!(text.contains(k.as_str().unwrap()));
            }
        }
        let s = &v["summary"];
        assert!(
            text.contains(&format!(
                "summary: {} error, {} warning, {} info; 11 checks",
                s["error"], s["warning"], s["info"]
            )),
            "{text}"
        );
        assert!(text.contains("next: "), "{text}");
        // The versions are printed.
        let ver = &v["versions"];
        assert!(text.contains(&format!("shard format {}", ver["shard_format"])));
        assert!(text.contains(&format!("cache schema {}", ver["cache_schema"])));
    }

    fn ok_or_fail(dir: &Path, args: &[&str]) -> String {
        String::from_utf8(katalogr(dir, args).stdout).unwrap()
    }
}

mod doctor {
    use super::*;

    /// Findings in many areas, listed with `--verbose`: no path of the machine appears.
    #[test]
    fn output_has_no_absolute_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = healthy(tmp.path());
        let project = kx.join("projects/demo");
        // Indexing sets the undecodable shard aside and records the refused profile.
        std::fs::write(project.join("shards/app/junk.kshard.json"), "x").unwrap();
        std::fs::create_dir_all(project.join("profiles")).unwrap();
        std::fs::write(
            project.join("profiles/bad.yaml"),
            "profile: p\nversion: 3\nrules: []\n",
        )
        .unwrap();
        ok(&repo, &["index"]);
        // What indexing would fix, left behind.
        dirty(&kx);
        std::fs::write(project.join("shards/app/.tmp-x.kshard.json"), "{}").unwrap();
        std::fs::write(project.join("shards/app/new.tx.kshard.json"), "{}").unwrap();
        std::fs::create_dir_all(project.join(".cache/inbox")).unwrap();
        std::fs::write(project.join(".cache/inbox/1-1-aa.json.bad"), "x").unwrap();
        std::fs::write(project.join(".cache/roots.yaml"), "{}\n").unwrap();
        // Run from a directory with no marker so the repo has no root either.
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let kxs = kx.to_str().unwrap();
        let mut forbidden = vec![tmp.path().to_str().unwrap().to_string()];
        forbidden.push(
            std::fs::canonicalize(tmp.path())
                .unwrap()
                .to_str()
                .unwrap()
                .to_string(),
        );
        if let Some(home) = std::env::var_os("HOME") {
            forbidden.push(home.to_str().unwrap().to_string());
        }
        forbidden.retain(|f| f.len() > 1);
        let mut saw = Vec::new();
        for args in [
            vec!["--kindex", kxs, "--project", "demo", "doctor", "--verbose"],
            vec![
                "--kindex",
                kxs,
                "--project",
                "demo",
                "--json",
                "doctor",
                "--verbose",
            ],
        ] {
            let out = katalogr(&elsewhere, &args);
            assert_eq!(out.status.code(), Some(1));
            let text = String::from_utf8(out.stdout).unwrap();
            for f in &forbidden {
                assert!(!text.contains(f.as_str()), "{f} in\n{text}");
            }
            assert!(
                !text.contains("/home/") && !text.contains("/tmp/"),
                "{text}"
            );
            saw.push(text);
        }
        for code in [
            "KD-SHD-001",
            "KD-JNL-003",
            "KD-QUE-003",
            "KD-PRF-001",
            "KD-ROOT-001",
            "KD-GIT-001",
        ] {
            assert!(saw[0].contains(code), "{code} missing from\n{}", saw[0]);
        }
    }
}
