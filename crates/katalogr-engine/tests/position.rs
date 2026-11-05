//! FEAT-084 T1 and T10: reading a checkout's position, and the shared contract file.

mod position {
    use katalogr_engine::position::{dirty_hash, read_position};
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::process::Command;

    const A_CHANGED_ID: &str = "5ea2ed416fbd4a4cbe227b75fe255dd7fa6bd4d6";

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

    fn put(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// A committed repo: `.gitignore`, `a.txt`, `src/b.txt`.
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        git(r, &["init", "-q"]);
        git(r, &["config", "user.name", "Test"]);
        git(r, &["config", "user.email", "test@example.com"]);
        git(r, &["config", "commit.gpgsign", "false"]);
        put(r, ".gitignore", "*.log\n");
        put(r, "a.txt", "one\n");
        put(r, "src/b.txt", "two\n");
        git(r, &["add", "."]);
        git(r, &["commit", "-q", "-m", "c1"]);
        dir
    }

    fn pos(root: &Path) -> katalogr_query::PositionRef {
        let inc = vec!["**".to_string()];
        read_position("r", root, &inc, &[]).unwrap().position
    }

    fn head(root: &Path) -> String {
        let out = Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn clean_has_no_dirty_hash() {
        let d = repo();
        let p = pos(d.path());
        assert_eq!(p.commit.as_deref(), Some(head(d.path()).as_str()));
        assert_eq!(p.dirty, None);
        assert_eq!(p.text(), head(d.path()));
    }

    #[test]
    fn hash_matches_spec_vector() {
        // One edited path: a.txt now holds "changed\n".
        assert_eq!(
            dirty_hash([(b"a.txt".as_slice(), Some(A_CHANGED_ID))]).as_deref(),
            Some("83ebb47e03fee94773edcd621a87f37980b3b408b96467062fcc87368f3260c7")
        );
        // A deleted path uses `-`.
        assert_eq!(
            dirty_hash([(b"src/b.txt".as_slice(), None)]).as_deref(),
            Some("c09b5a87b6536d1a72e30c97cfd47a03df6b36c40af129dabe9947c4f99b0ce0")
        );
        assert_eq!(dirty_hash([]), None);
        // Input order does not matter.
        let one = dirty_hash([
            (b"a.txt".as_slice(), Some(A_CHANGED_ID)),
            (b"src/b.txt".as_slice(), None),
        ]);
        let two = dirty_hash([
            (b"src/b.txt".as_slice(), None),
            (b"a.txt".as_slice(), Some(A_CHANGED_ID)),
        ]);
        assert_eq!(one, two);
    }

    #[test]
    fn edit_changes_hash() {
        let d = repo();
        put(d.path(), "a.txt", "changed\n");
        let st = read_position("r", d.path(), &[], &[]).unwrap();
        assert_eq!(
            st.position.dirty.as_deref(),
            Some("83ebb47e03fee94773edcd621a87f37980b3b408b96467062fcc87368f3260c7")
        );
        assert_eq!(st.changed.len(), 1);
        assert_eq!(st.changed[0].0.as_str(), "a.txt");
        assert_eq!(st.changed[0].1.as_deref(), Some(A_CHANGED_ID));
        put(d.path(), "a.txt", "changed again\n");
        assert_ne!(pos(d.path()).dirty, st.position.dirty);
    }

    #[test]
    fn staging_alone_changes_nothing() {
        let d = repo();
        let before = pos(d.path());
        put(d.path(), "a.txt", "changed\n");
        let edited = pos(d.path());
        git(d.path(), &["add", "a.txt"]);
        assert_eq!(pos(d.path()), edited);
        // Staging a change and then restoring the file leaves the position clean.
        put(d.path(), "a.txt", "one\n");
        assert_eq!(pos(d.path()), before);
    }

    #[test]
    fn untracked_counts_ignored_does_not() {
        let d = repo();
        let clean = pos(d.path());
        put(d.path(), "x.log", "noise\n");
        assert_eq!(pos(d.path()), clean);
        put(d.path(), "new.txt", "new\n");
        let st = read_position("r", d.path(), &[], &[]).unwrap();
        assert_eq!(
            st.position.dirty.as_deref(),
            Some("d12b81ae5c979f72e6d12ad0d7c884a29fb2c5d08c3a442782e4fc708253a544")
        );
        // A new file in a new directory is found as well.
        put(d.path(), "deep/er/c.txt", "c\n");
        let st = read_position("r", d.path(), &[], &[]).unwrap();
        let paths: Vec<&str> = st.changed.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["deep/er/c.txt", "new.txt"]);
    }

    #[test]
    fn deleted_uses_dash() {
        let d = repo();
        std::fs::remove_file(d.path().join("src/b.txt")).unwrap();
        let st = read_position("r", d.path(), &[], &[]).unwrap();
        assert_eq!(
            st.position.dirty.as_deref(),
            Some("c09b5a87b6536d1a72e30c97cfd47a03df6b36c40af129dabe9947c4f99b0ce0")
        );
        assert_eq!(st.changed.len(), 1);
        assert_eq!(st.changed[0].0.as_str(), "src/b.txt");
        assert_eq!(st.changed[0].1, None);
    }

    #[test]
    fn revert_to_head_is_clean() {
        let d = repo();
        put(d.path(), "a.txt", "changed\n");
        assert!(pos(d.path()).dirty.is_some());
        put(d.path(), "a.txt", "one\n");
        assert_eq!(pos(d.path()).dirty, None);
    }

    #[cfg(unix)]
    #[test]
    fn mode_change_does_not_count() {
        use std::os::unix::fs::PermissionsExt;
        let d = repo();
        let p = d.path().join("a.txt");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(pos(d.path()).dirty, None);
    }

    #[test]
    fn plain_directory_has_no_commit() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "a.txt", "one\n");
        put(dir.path(), "sub/b.txt", "two\n");
        put(dir.path(), "skip.bin", "zz\n");
        let inc = vec!["**/*.txt".to_string()];
        let st = read_position("r", dir.path(), &inc, &[]).unwrap();
        assert_eq!(st.position.commit, None);
        let dirty = st.position.dirty.clone().expect("dirty hash");
        assert_eq!(st.position.text(), format!("none+{dirty}"));
        assert_eq!(st.changed.len(), 2);
        // A file the globs do not admit does not count.
        put(dir.path(), "skip.bin", "other\n");
        assert_eq!(
            read_position("r", dir.path(), &inc, &[]).unwrap().position,
            st.position
        );
        // An unborn HEAD is commit None too.
        let g = tempfile::tempdir().unwrap();
        git(g.path(), &["init", "-q"]);
        put(g.path(), "a.txt", "one\n");
        let p = read_position("r", g.path(), &[], &[]).unwrap().position;
        assert_eq!(p.commit, None);
        assert!(p.dirty.is_some());
    }

    // ---- T10: the shared contract file (R-14) ----

    fn contract() -> Value {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/contract/position-v1.json");
        serde_json::from_slice(&std::fs::read(path).expect("read position-v1.json"))
            .expect("parse position-v1.json")
    }

    fn files(v: &Value) -> BTreeMap<String, String> {
        v.as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn shared_file_covers_required_cases() {
        let v = contract();
        assert_eq!(v["format"], 1);
        let names: Vec<&str> = v["cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        for need in [
            "clean",
            "edited",
            "deleted",
            "untracked",
            "staged_only",
            "line_endings",
        ] {
            assert!(names.contains(&need), "missing case {need}: {names:?}");
        }
        let unique: std::collections::BTreeSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "duplicate case names");
        for c in v["cases"].as_array().unwrap() {
            let name = c["name"].as_str().unwrap();
            assert!(c["head"].is_object() && c["worktree"].is_object(), "{name}");
            let expected = c["expected"].as_str().unwrap();
            let parsed = katalogr_query::PositionRef::parse("r", expected)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(parsed.text(), expected, "{name}");
        }
    }

    #[test]
    fn shared_file_cases_match() {
        let v = contract();
        let author = v["commit"]["author"].as_str().unwrap();
        let (name, email) = author.split_once(" <").unwrap();
        let email = email.trim_end_matches('>');
        // git reads a bare unix time with a zone as `@<secs> <zone>`.
        let time = format!("@{}", v["commit"]["time"].as_str().unwrap());
        let message = v["commit"]["message"].as_str().unwrap();
        let mut failures = Vec::new();
        for c in v["cases"].as_array().unwrap() {
            let case = c["name"].as_str().unwrap();
            let d = tempfile::tempdir().unwrap();
            let r = d.path();
            git(r, &["init", "-q"]);
            for (path, text) in files(&c["head"]) {
                put(r, &path, &text);
            }
            git(r, &["add", "-A"]);
            let st = Command::new("git")
                .current_dir(r)
                .args(["commit", "-q", "--no-gpg-sign", "-F", "-"])
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", name)
                .env("GIT_AUTHOR_EMAIL", email)
                .env("GIT_AUTHOR_DATE", &time)
                .env("GIT_COMMITTER_NAME", name)
                .env("GIT_COMMITTER_EMAIL", email)
                .env("GIT_COMMITTER_DATE", &time)
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut ch| {
                    use std::io::Write;
                    ch.stdin.take().unwrap().write_all(message.as_bytes())?;
                    ch.wait()
                })
                .unwrap();
            assert!(st.success(), "{case}: commit failed");
            if let Some(cfg) = c["config"].as_object() {
                for (k, val) in cfg {
                    git(r, &["config", k, val.as_str().unwrap()]);
                }
            }
            // The worktree is exactly the listed files: delete what it omits.
            let want = files(&c["worktree"]);
            for (path, _) in files(&c["head"]) {
                if !want.contains_key(&path) {
                    std::fs::remove_file(r.join(&path)).unwrap();
                }
            }
            for (path, text) in &want {
                put(r, path, text);
            }
            for (path, text) in files(&c["index"]) {
                // Stage the entry without touching the worktree file.
                let blob = Command::new("git")
                    .current_dir(r)
                    .args(["hash-object", "-w", "--stdin"])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .and_then(|mut ch| {
                        use std::io::Write;
                        ch.stdin.take().unwrap().write_all(text.as_bytes())?;
                        ch.wait_with_output()
                    })
                    .unwrap();
                let id = String::from_utf8(blob.stdout).unwrap().trim().to_string();
                git(
                    r,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("100644,{id},{path}"),
                    ],
                );
            }
            let got = pos(r).text();
            let expected = c["expected"].as_str().unwrap();
            if got != expected {
                failures.push(format!("{case}: got {got}, expected {expected}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
