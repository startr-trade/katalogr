//! FEAT-141: a checkout whose text files have CRLF line endings because git converts them on
//! checkout (`core.autocrlf=true`, or `.gitattributes` `eol=crlf`) is clean, the way `git status`
//! decides it. The conversion is forced through the test repository's own config, so these run
//! the same on every OS.

mod position_eol {
    use katalogr_engine::position::{
        load_stat_cache, read_position, read_position_cached, reference_position,
    };
    use katalogr_engine::scan::changed_hunks;
    use katalogr_store::blob::git_blob_id;
    use std::path::Path;
    use std::process::Command;

    fn git_out(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn git(root: &Path, args: &[&str]) {
        git_out(root, args);
    }

    fn put(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    /// A binary file: a NUL byte and CRLF pairs, which `text=auto` must leave alone.
    const BINARY: &[u8] = b"\x00\x01bin\r\nary\r\n\x02";
    const TEXT: [(&str, &str); 3] = [
        ("a.txt", "one\ntwo\n"),
        ("src/lib.rs", "pub fn f() -> u32 {\n    1\n}\n"),
        ("docs/guide.md", "# Guide\n\nSee `f`.\n"),
    ];

    /// How the checkout comes to have CRLF files.
    #[derive(Clone, Copy, Debug)]
    enum Eol {
        /// `core.autocrlf=true` in the repository config.
        AutoCrlf,
        /// `.gitattributes` with `* text=auto eol=crlf`.
        Attributes,
    }

    /// A committed repo with LF blobs and a binary, checked out again with the conversion
    /// configured, so its text files have CRLF.
    fn crlf_checkout(eol: Eol) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        git(r, &["init", "-q"]);
        for (k, v) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            git(r, &["config", k, v]);
        }
        if let Eol::Attributes = eol {
            put(r, ".gitattributes", b"* text=auto eol=crlf\n");
        }
        for (p, t) in TEXT {
            put(r, p, t.as_bytes());
        }
        put(r, "bin.dat", BINARY);
        git(r, &["add", "."]);
        git(r, &["commit", "-q", "-m", "c1"]);
        if let Eol::AutoCrlf = eol {
            git(r, &["config", "core.autocrlf", "true"]);
        }
        // Check out again: git writes the text files with CRLF, the binary as it is.
        let mut paths: Vec<&str> = TEXT.iter().map(|(p, _)| *p).collect();
        paths.push("bin.dat");
        if let Eol::Attributes = eol {
            paths.push(".gitattributes");
        }
        for p in &paths {
            std::fs::remove_file(r.join(p)).unwrap();
        }
        git(r, &["checkout", "--", "."]);
        for (p, t) in TEXT {
            let have = std::fs::read(r.join(p)).unwrap();
            assert_eq!(
                have,
                t.replace('\n', "\r\n").as_bytes(),
                "{eol:?}: {p} has CRLF"
            );
        }
        assert_eq!(std::fs::read(r.join("bin.dat")).unwrap(), BINARY);
        // Git itself agrees the checkout is clean.
        assert_eq!(git_out(r, &["status", "--porcelain"]), "", "{eol:?}");
        dir
    }

    fn inc() -> Vec<String> {
        vec!["**".to_string()]
    }

    fn head(root: &Path) -> String {
        git_out(root, &["rev-parse", "HEAD"]).trim().to_string()
    }

    /// What `git hash-object` stores for the file, with the repository's conversion.
    fn git_hash(root: &Path, rel: &str) -> String {
        git_out(root, &["hash-object", rel]).trim().to_string()
    }

    /// FEAT-141 acceptance: a CRLF checkout's position is clean, through the cached reader (cold
    /// and warm), the uncached reader and the reference.
    #[test]
    fn crlf_checkout_is_clean() {
        for eol in [Eol::AutoCrlf, Eol::Attributes] {
            let d = crlf_checkout(eol);
            let r = d.path();
            let cache = tempfile::tempdir().unwrap();
            let file = cache.path().join("r.stat");
            for _ in 0..2 {
                let st = read_position_cached("r", r, &inc(), &[], Some(&file)).unwrap();
                assert_eq!(st.position.dirty, None, "{eol:?}: {:?}", st.changed);
                assert_eq!(st.position.commit.as_deref(), Some(head(r).as_str()));
            }
            assert_eq!(
                read_position("r", r, &inc(), &[]).unwrap().position.dirty,
                None
            );
            let refp = reference_position("r", r, &inc(), &[]).unwrap();
            assert_eq!(refp.position.dirty, None, "{eol:?}: {:?}", refp.changed);
        }
    }

    /// FEAT-141 acceptance: `changed_hunks` against the working tree (what `docs-affected` and
    /// `diff` read) reports nothing for a CRLF checkout.
    #[test]
    fn crlf_checkout_has_no_changed_hunks() {
        for eol in [Eol::AutoCrlf, Eol::Attributes] {
            let d = crlf_checkout(eol);
            let h = head(d.path());
            let changes = changed_hunks(d.path(), &h, None).unwrap();
            assert!(changes.is_empty(), "{eol:?}: {changes:?}");
        }
    }

    /// FEAT-141 acceptance: a real edit to a CRLF file is still a change, with the id git would
    /// store (the LF form), and its changed lines are the edited ones only.
    #[test]
    fn real_edit_is_still_detected() {
        for eol in [Eol::AutoCrlf, Eol::Attributes] {
            let d = crlf_checkout(eol);
            let r = d.path();
            put(r, "src/lib.rs", b"pub fn f() -> u32 {\r\n    2\r\n}\r\n");
            let cache = tempfile::tempdir().unwrap();
            let file = cache.path().join("r.stat");
            let st = read_position_cached("r", r, &inc(), &[], Some(&file)).unwrap();
            assert!(st.position.dirty.is_some(), "{eol:?}");
            let paths: Vec<&str> = st.changed.iter().map(|(p, _)| p.as_str()).collect();
            assert_eq!(paths, ["src/lib.rs"], "{eol:?}");
            let id = st.changed[0].1.clone().unwrap();
            assert_eq!(id, git_hash(r, "src/lib.rs"), "{eol:?}");
            assert_eq!(id, git_blob_id(b"pub fn f() -> u32 {\n    2\n}\n"));
            let refp = reference_position("r", r, &inc(), &[]).unwrap();
            assert_eq!(refp.position, st.position, "{eol:?}");

            let changes = changed_hunks(r, &head(r), None).unwrap();
            assert_eq!(changes.len(), 1, "{eol:?}: {changes:?}");
            assert_eq!(changes[0].path.as_str(), "src/lib.rs");
            assert_eq!(changes[0].ranges, vec![(2, 2)], "{eol:?}");

            // An untracked CRLF text file counts with its repository-form id.
            put(r, "new.txt", b"x\r\ny\r\n");
            let st = read_position_cached("r", r, &inc(), &[], Some(&file)).unwrap();
            let new = st
                .changed
                .iter()
                .find(|(p, _)| p.as_str() == "new.txt")
                .unwrap();
            assert_eq!(new.1.as_deref(), Some(git_hash(r, "new.txt").as_str()));
            assert_eq!(new.1.as_deref(), Some(git_blob_id(b"x\ny\n").as_str()));
        }
    }

    /// FEAT-141: a binary file with CRLF bytes is not converted; its id is its raw-bytes id,
    /// tracked or untracked, as git stores it.
    #[test]
    fn binary_with_crlf_is_not_converted() {
        for eol in [Eol::AutoCrlf, Eol::Attributes] {
            let d = crlf_checkout(eol);
            let r = d.path();
            let edited = b"\x00\x03other\r\nbytes\r\n";
            put(r, "bin.dat", edited);
            put(r, "new.bin", BINARY);
            let st = read_position("r", r, &inc(), &[]).unwrap();
            let id = |rel: &str| {
                st.changed
                    .iter()
                    .find(|(p, _)| p.as_str() == rel)
                    .and_then(|(_, b)| b.clone())
                    .unwrap_or_else(|| panic!("{rel} not changed: {:?}", st.changed))
            };
            assert_eq!(id("bin.dat"), git_blob_id(edited), "{eol:?}");
            assert_eq!(id("bin.dat"), git_hash(r, "bin.dat"), "{eol:?}");
            assert_eq!(id("new.bin"), git_blob_id(BINARY), "{eol:?}");
            assert_eq!(st.changed.len(), 2, "{eol:?}: {:?}", st.changed);
        }
    }

    /// FEAT-141: a stat cache written before the conversion was configured is not trusted after:
    /// turning on `core.autocrlf` makes the same CRLF files clean without touching them.
    #[test]
    fn conversion_settings_change_rereads_cached_files() {
        let d = crlf_checkout(Eol::AutoCrlf);
        let r = d.path();
        git(r, &["config", "core.autocrlf", "false"]);
        let cache = tempfile::tempdir().unwrap();
        let file = cache.path().join("r.stat");
        // Make every stat old enough to be trusted by the next read.
        std::thread::sleep(std::time::Duration::from_millis(2100));
        let st = read_position_cached("r", r, &inc(), &[], Some(&file)).unwrap();
        assert_eq!(
            st.changed.len(),
            TEXT.len(),
            "raw CRLF bytes differ: {:?}",
            st.changed
        );
        git(r, &["config", "core.autocrlf", "true"]);
        let st = read_position_cached("r", r, &inc(), &[], Some(&file)).unwrap();
        assert_eq!(st.position.dirty, None, "{:?}", st.changed);
    }

    /// FEAT-141: with no conversion configured, an LF checkout's ids are the raw-bytes ids (the
    /// conversion is the identity), so positions recorded before keep their values.
    #[test]
    fn lf_checkout_ids_are_raw_bytes_ids() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        git(r, &["init", "-q"]);
        git(r, &["config", "user.name", "Test"]);
        git(r, &["config", "user.email", "test@example.com"]);
        git(r, &["config", "commit.gpgsign", "false"]);
        git(r, &["config", "core.autocrlf", "false"]);
        put(r, "a.txt", b"one\n");
        git(r, &["add", "."]);
        git(r, &["commit", "-q", "-m", "c1"]);
        // A CRLF file committed and edited on an LF checkout keeps its CR bytes.
        put(r, "a.txt", b"one\r\nmore\r\n");
        put(r, "b.txt", b"b\r\n");
        let st = read_position("r", r, &inc(), &[]).unwrap();
        let ids: Vec<(&str, String)> = st
            .changed
            .iter()
            .map(|(p, b)| (p.as_str(), b.clone().unwrap()))
            .collect();
        assert_eq!(
            ids,
            vec![
                ("a.txt", git_blob_id(b"one\r\nmore\r\n")),
                ("b.txt", git_blob_id(b"b\r\n")),
            ]
        );
    }

    /// Sets the file's mtime an hour back, so a stat cache written now trusts it.
    fn age(p: &Path) {
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::OpenOptions::new()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    /// FEAT-141 regression (the Windows `cache_equals_full_hash` failure): under `core.autocrlf`
    /// a CRLF file's repository form depends on the index (git's `text=auto` rule keeps CRLF when
    /// the index blob already has it), so staging alone, with the file, attributes and config
    /// untouched, must re-read a cached conversion. The git that stages ignores the repo's
    /// `core.autocrlf` (as a Windows runner's system config is read by gix only), so the CRLF
    /// bytes are staged as they are.
    #[test]
    fn staging_crlf_file_rereads_cached_conversion() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        git(r, &["init", "-q"]);
        for (k, v) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "true"),
        ] {
            git(r, &["config", k, v]);
        }
        put(r, "a.txt", b"one\n");
        put(r, "b.txt", b"lf only\n");
        git(r, &["add", "."]);
        git(r, &["commit", "-q", "-m", "c1"]);
        let cache = tempfile::tempdir().unwrap();
        let file = cache.path().join("r.stat");
        let canon = std::fs::canonicalize(r).unwrap().display().to_string();
        let cached = || {
            read_position_cached("r", r, &inc(), &[], Some(&file))
                .unwrap()
                .position
        };
        let reference = || reference_position("r", r, &inc(), &[]).unwrap().position;

        // An LF checkout never depends on the index: staging alone keeps the cache.
        put(r, "b.txt", b"lf edit\n");
        age(&r.join("b.txt"));
        assert_eq!(cached(), reference());
        let c = load_stat_cache(&file, &canon).expect("cache written");
        assert_eq!(c.index_key, None, "LF files never consult the index");

        // The CRLF working file converts to the committed LF blob: clean apart from b.txt.
        put(r, "a.txt", b"one\r\n");
        age(&r.join("a.txt"));
        let before = reference();
        assert_eq!(cached(), before, "cold");
        assert_eq!(cached(), before, "warm");
        assert!(
            load_stat_cache(&file, &canon).unwrap().index_key.is_some(),
            "the CRLF conversion consulted the index"
        );

        // Staged with its CRLF: the index blob now has CRLF, so the file is no longer
        // normalised and a.txt is dirty, although nothing in the work tree changed.
        git(r, &["-c", "core.autocrlf=false", "add", "a.txt"]);
        let after = reference();
        assert_ne!(after, before, "the index rule changes the form");
        assert_eq!(cached(), after, "cached after staging");
        assert_eq!(cached(), after, "cached again");
        assert_eq!(
            read_position("r", r, &inc(), &[]).unwrap().position,
            after,
            "uncached"
        );
    }
}
