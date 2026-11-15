//! FEAT-084 T1 and T10: reading a checkout's position, and the shared contract file.

mod support;

mod position {
    use katalogr_engine::position::{
        dirty_hash, load_stat_cache, read_position, read_position_cached, reference_position,
        stat_of,
    };
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
    fn ignored_file_edit_keeps_same_answer() {
        // FEAT-099: a file git ignores neither shows in the scan nor moves the position.
        let d = repo();
        let before = pos(d.path());
        put(d.path(), "x.log", "noise\n");
        let listed = |root: &Path| {
            katalogr_engine::scan::list_files(root, &["**".to_string()], &[]).unwrap()
        };
        assert!(listed(d.path()).iter().all(|p| p.as_str() != "x.log"));
        put(d.path(), "x.log", "noise, edited\n");
        assert_eq!(pos(d.path()), before);
        assert!(listed(d.path()).iter().all(|p| p.as_str() != "x.log"));
        // The same holds through the cached reader and the reference.
        let inc = vec!["**".to_string()];
        let r = reference_position("r", d.path(), &inc, &[])
            .unwrap()
            .position;
        assert_eq!(r, before);
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

    // ---- the stat cache (FEAT-084 revision) ----

    fn age(root: &Path) {
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        fn walk(dir: &Path, old: std::time::SystemTime) {
            for e in std::fs::read_dir(dir).unwrap() {
                let e = e.unwrap();
                if e.file_name() == ".git" {
                    continue;
                }
                let p = e.path();
                if p.is_dir() {
                    walk(&p, old);
                }
                crate::support::set_mtime(&p, old);
            }
        }
        walk(root, old);
        crate::support::set_mtime(root, old);
    }

    fn inc() -> Vec<String> {
        vec!["**".to_string()]
    }

    fn cached(root: &Path, cache: &Path) -> katalogr_query::PositionRef {
        read_position_cached("r", root, &inc(), &[], Some(cache))
            .unwrap()
            .position
    }

    fn reference(root: &Path) -> katalogr_query::PositionRef {
        reference_position("r", root, &inc(), &[]).unwrap().position
    }

    #[test]
    fn cache_equals_full_hash() {
        cache_equals_full_hash_with(false);
    }

    /// FEAT-141. The same random steps with `core.autocrlf=true`, as Git for Windows sets it:
    /// the conversion is active and a CRLF file's form depends on the index.
    #[test]
    fn cache_equals_full_hash_autocrlf() {
        cache_equals_full_hash_with(true);
    }

    fn cache_equals_full_hash_with(autocrlf: bool) {
        let d = repo();
        let r = d.path();
        // katalogr reads `core.autocrlf=true`; the git that stages and commits does not, as on a
        // Windows runner whose system config only gix reads. A CRLF file is then staged as CRLF,
        // and git's index rule (`text=auto`: a CRLF blob in the index is not normalised) changes
        // the file's repository form on staging alone.
        let mut add_all: Vec<&str> = vec!["add", "-A"];
        let mut commit: Vec<&str> = vec!["commit", "-q", "--allow-empty", "-m", "step"];
        if autocrlf {
            git(r, &["config", "core.autocrlf", "true"]);
            for args in [&mut add_all, &mut commit] {
                args.splice(0..0, ["-c", "core.autocrlf=false"]);
            }
        }
        let cdir = tempfile::tempdir().unwrap();
        let cache = cdir.path().join("p/r.stat");
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let names = [
            "a.txt",
            "src/b.txt",
            "c/d.txt",
            "e.txt",
            "src/deep/f.txt",
            "x.log",
            "g.dat",
        ];
        for step in 0..60 {
            let name = names[next(names.len() as u64) as usize];
            let what = match next(9) {
                0 | 1 => {
                    put(r, name, &format!("content {step}\n"));
                    "write"
                }
                2 => {
                    let _ = std::fs::remove_file(r.join(name));
                    "delete"
                }
                3 => {
                    put(r, name, "same size\n");
                    "write fixed"
                }
                4 => {
                    // A tracked file with line endings changed.
                    put(r, "a.txt", "one\r\n");
                    "crlf"
                }
                5 => {
                    let rule = ["*.dat\n", "*.log\n", "e.*\n"][next(3) as usize];
                    put(r, ".gitignore", rule);
                    "ignore rule"
                }
                6 => {
                    put(r, "src/.gitignore", "*.txt\n");
                    "nested ignore"
                }
                7 => {
                    git(r, &add_all);
                    "stage"
                }
                _ => {
                    git(r, &add_all);
                    git(r, &commit);
                    "commit"
                }
            };
            if next(3) == 0 {
                age(r);
            }
            let want = reference(r);
            let got = cached(r, &cache);
            assert_eq!(got, want, "step {step} ({what} {name})");
            // A second read, from the cache just written, agrees as well.
            assert_eq!(cached(r, &cache), want, "step {step} again ({what} {name})");
            assert_eq!(pos(r), want, "step {step} uncached ({what} {name})");
        }
    }

    #[test]
    fn racy_same_size_edit_seen() {
        let d = repo();
        let r = d.path();
        age(r);
        let cdir = tempfile::tempdir().unwrap();
        let cache = cdir.path().join("r.stat");
        assert_eq!(cached(r, &cache).dirty, None);
        // A same-size edit. The cache is made to hold the file's new stat with the old verdict,
        // as if the edit had kept size, mtime, ctime and inode.
        put(r, "a.txt", "uno\n");
        let cur = stat_of(&r.join("a.txt")).unwrap();
        let canon = std::fs::canonicalize(r).unwrap().display().to_string();
        let mut c = load_stat_cache(&cache, &canon).expect("cache written");
        c.tracked.get_mut("a.txt").unwrap().stat = Some(cur);
        // Written within 2 s of the file's mtime: racy, so the file is read again.
        c.written_ns = cur.mtime_ns + 1_000_000_000;
        std::fs::write(&cache, serde_json::to_vec(&c).unwrap()).unwrap();
        assert!(cached(r, &cache).dirty.is_some(), "racy edit missed");
        // Control: written long after the mtime, the same stat is trusted (git's own trust).
        let mut c = load_stat_cache(&cache, &canon).unwrap();
        c.tracked.get_mut("a.txt").unwrap().stat = Some(cur);
        c.tracked.get_mut("a.txt").unwrap().raw = katalogr_engine::position::Raw::Same;
        c.written_ns = cur.mtime_ns + 10_000_000_000;
        std::fs::write(&cache, serde_json::to_vec(&c).unwrap()).unwrap();
        assert_eq!(
            cached(r, &cache).dirty,
            None,
            "an unchanged stat is trusted"
        );
    }

    #[test]
    fn corrupt_cache_rebuilds() {
        let d = repo();
        let r = d.path();
        put(r, "a.txt", "changed\n");
        let cdir = tempfile::tempdir().unwrap();
        let cache = cdir.path().join("r.stat");
        let want = reference(r);
        for junk in [&b"not json"[..], b"", b"{\"version\":99}", b"\xff\xfe"] {
            std::fs::write(&cache, junk).unwrap();
            assert_eq!(cached(r, &cache), want);
            let canon = std::fs::canonicalize(r).unwrap().display().to_string();
            assert!(load_stat_cache(&cache, &canon).is_some(), "rebuilt");
        }
        // An unwritable cache location only costs the savings.
        let blocked = cdir.path().join("file");
        std::fs::write(&blocked, "x").unwrap();
        assert_eq!(cached(r, &blocked.join("sub/r.stat")), want);
    }

    #[test]
    fn new_ignore_rule_rewalks() {
        let d = repo();
        let r = d.path();
        put(r, "sub/k.dat", "k\n");
        age(r);
        let cdir = tempfile::tempdir().unwrap();
        let cache = cdir.path().join("r.stat");
        let before = cached(r, &cache);
        assert!(before.dirty.is_some());
        // `sub` itself is not touched, so only the ignore inputs say the rule is new.
        let mut ig = std::fs::read_to_string(r.join(".gitignore")).unwrap();
        ig.push_str("*.dat\n");
        std::fs::write(r.join(".gitignore"), &ig).unwrap();
        let st = read_position_cached("r", r, &inc(), &[], Some(&cache)).unwrap();
        assert_eq!(st.position, reference(r));
        assert!(st.changed.iter().all(|(p, _)| p.as_str() != "sub/k.dat"));
        // Likewise a rule in `.git/info/exclude`.
        put(r, "sub/m.cfg", "m\n");
        age(r);
        let _ = cached(r, &cache);
        std::fs::write(r.join(".git/info/exclude"), "*.cfg\n").unwrap();
        let st = read_position_cached("r", r, &inc(), &[], Some(&cache)).unwrap();
        assert_eq!(st.position, reference(r));
        assert!(st.changed.iter().all(|(p, _)| p.as_str() != "sub/m.cfg"));
    }

    #[test]
    fn new_untracked_in_cached_dir_seen() {
        let d = repo();
        let r = d.path();
        age(r);
        let cdir = tempfile::tempdir().unwrap();
        let cache = cdir.path().join("r.stat");
        assert_eq!(cached(r, &cache).dirty, None);
        put(r, "src/new.txt", "new\n");
        let st = read_position_cached("r", r, &inc(), &[], Some(&cache)).unwrap();
        assert!(st.changed.iter().any(|(p, _)| p.as_str() == "src/new.txt"));
        assert_eq!(st.position, reference(r));
        // Removing it again returns to clean.
        std::fs::remove_file(r.join("src/new.txt")).unwrap();
        assert_eq!(cached(r, &cache).dirty, None);
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
            let expected = c["expected"].as_str().unwrap();
            // Cold (no cache), then with a cache that is empty, then with it warm.
            let cache_dir = tempfile::tempdir().unwrap();
            let cache = cache_dir.path().join("r.stat");
            let inc = vec!["**".to_string()];
            let cached = || {
                read_position_cached("r", r, &inc, &[], Some(&cache))
                    .unwrap()
                    .position
                    .text()
            };
            for (how, got) in [
                ("cold", pos(r).text()),
                ("cache empty", cached()),
                ("cache warm", cached()),
                ("cache warm again", cached()),
            ] {
                if got != expected {
                    failures.push(format!("{case} ({how}): got {got}, expected {expected}"));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// FEAT-097 R-13. A repo with no root here but a recorded position answers from its rows
    /// and counts as exact at that position.
    #[test]
    fn rootless_recorded_repo_is_exact() {
        use camino::Utf8PathBuf;
        use katalogr_engine::registry::ExtensionRegistry;
        use katalogr_engine::{Consistency, Engine, QueryOptions, Scope};
        use std::collections::{BTreeMap, BTreeSet};
        use std::time::{Duration, Instant};

        let tmp = tempfile::tempdir().unwrap();
        let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        crate::support::init_project(&kx);
        let dir = || {
            katalogr_store::Kindex::open(&kx)
                .unwrap()
                .project(&katalogr_store::ProjectName::new("p").unwrap())
                .unwrap()
        };
        let commit = "ab".repeat(20);
        let open = |recorded_only: bool| {
            let mut e = Engine::open(
                dir(),
                ExtensionRegistry::new(),
                BTreeMap::new(),
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap();
            if recorded_only {
                e.set_recorded_only(BTreeSet::from(["r".to_string()]));
            }
            e
        };
        let read = |e: &Engine| {
            e.with_consistency(
                &QueryOptions {
                    consistency: Consistency::WaitAll,
                    timeout: Duration::from_secs(5),
                    scope: Scope::Project,
                },
                |_| (),
            )
            .unwrap()
        };
        {
            let e = open(false);
            let mut tx = e.cache().write().unwrap();
            let json = serde_json::json!({"repo": "r", "commit": commit, "dirty": null});
            tx.set_meta("position.r", json.to_string().as_bytes())
                .unwrap();
            tx.set_meta("position_changed.r", br#"{"over":false,"paths":[]}"#)
                .unwrap();
            tx.commit().unwrap();
            // Without the set, a project with no roots is not exact.
            let a = read(&e);
            assert!(!a.exact);
        }
        let e = open(true);
        let a = read(&e);
        assert!(a.exact, "{a:?}");
        assert!(!a.stale);
        assert_eq!(a.positions.len(), 1);
        assert_eq!(a.positions[0].repo, "r");
        assert_eq!(a.positions[0].commit.as_deref(), Some(commit.as_str()));
        assert_eq!(a.positions[0].dirty, None);
        drop(e);
        // A repo with no record is not exact merely by being listed.
        let mut e = open(false);
        e.set_recorded_only(BTreeSet::from(["unknown".to_string()]));
        assert!(!read(&e).exact);
    }
}
