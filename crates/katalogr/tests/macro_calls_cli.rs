//! FEAT-120 (R-4): a call made inside `assert_eq!` is listed by `katalogr tests` and
//! `katalogr callers`, through the real binary on a synthetic repo built in the test.

use std::path::{Path, PathBuf};
use std::process::Command;

const LIB: &str = "pub fn record(log: &mut Vec<i32>, n: i32) -> i32 {\n    log.push(n);\n    1\n}\n\n\
    #[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn t() {\n        \
    let mut l = Vec::new();\n        assert_eq!(record(&mut l, 5), 1);\n    }\n}\n";

fn run(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
        .current_dir(dir)
        .env_remove("KATALOGR_KINDEX")
        .env_remove("KATALOGR_PROJECT")
        .env_remove("KANBANR_DATA_DIR")
        .args(args)
        .output()
        .expect("run katalogr");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "{args:?}: {text}");
    text
}

/// A kindex over a one-file Rust repo, indexed. Returns the scratch dir and the kindex path.
fn indexed() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("app");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/lib.rs"), LIB).unwrap();
    let kx = tmp.path().join("kx");
    let kxs = kx.to_str().unwrap();
    run(tmp.path(), &["kindex", "init", kxs]);
    run(
        tmp.path(),
        &[
            "project",
            "--kindex",
            kxs,
            "init",
            "p",
            "--repo",
            &format!("app={}", repo.display()),
        ],
    );
    run(
        tmp.path(),
        &["index", "--kindex", kxs, "--project", "p", "--full"],
    );
    (tmp, kx)
}

fn query(cmd: &str) -> String {
    let (tmp, kx) = indexed();
    run(
        tmp.path(),
        &[
            cmd,
            "record",
            "--kindex",
            kx.to_str().unwrap(),
            "--project",
            "p",
        ],
    )
}

mod tests {
    #[test]
    fn assert_macro_test_found() {
        let out = super::query("tests");
        assert!(out.contains("fn:t"), "{out}");
    }
}

mod callers {
    #[test]
    fn assert_macro_caller_listed() {
        let out = super::query("callers");
        assert!(out.contains("fn:t"), "{out}");
    }
}
