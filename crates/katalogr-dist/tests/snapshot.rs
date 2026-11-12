//! FEAT-097: graph snapshot export and import through the facade. Needs the `.tx` test
//! extractor: `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod snapshot {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_dist::{DistError, Katalogr, Options, Overrides, kindex_init, project_init};
    use katalogr_store::{SnapshotManifest, StoreError, decode_snapshot, read_manifest};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub struct Setup {
        pub _tmp: tempfile::TempDir,
        pub kx: PathBuf,
        pub repo: PathBuf,
    }

    pub fn git(root: &Path, args: &[&str]) {
        let st = Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?} failed");
    }

    pub fn commit_all(repo: &Path, msg: &str) {
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", msg]);
    }

    /// A project of one repo `app` with two `.tx` files. With `with_git`, the repo has one
    /// commit; the index is built either way.
    pub fn setup_with(with_git: bool, ignore: Option<&str>) -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("s.tx"), "sym fn alpha\nsym fn beta\n").unwrap();
        std::fs::write(repo.join("t.tx"), "sym fn gamma\n").unwrap();
        if let Some(ig) = ignore {
            std::fs::write(repo.join(".gitignore"), ig).unwrap();
        }
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        if with_git {
            git(&repo, &["init", "-q"]);
            git(&repo, &["config", "user.name", "Test"]);
            git(&repo, &["config", "user.email", "test@example.com"]);
            git(&repo, &["config", "commit.gpgsign", "false"]);
            commit_all(&repo, "c1");
        }
        let s = Setup {
            _tmp: tmp,
            kx,
            repo,
        };
        katalogr(&s).index(None, true, None, &opts()).unwrap();
        s
    }

    pub fn setup() -> Setup {
        setup_with(true, None)
    }

    pub fn katalogr(s: &Setup) -> Katalogr {
        Katalogr::resolve(
            &s.repo,
            &Overrides {
                kindex: Some(s.kx.display().to_string()),
                project: None,
                ..Default::default()
            },
        )
        .unwrap()
    }

    pub fn opts() -> Options {
        Options {
            timeout: Some(20.0),
            ..Default::default()
        }
    }

    pub fn out_of(s: &Setup, name: &str) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(s._tmp.path().join(name)).unwrap()
    }

    pub fn cache_file(s: &Setup) -> PathBuf {
        s.kx.join("projects/demo/.cache/index.redb")
    }

    pub fn head(repo: &Path) -> String {
        let o = Command::new("git")
            .current_dir(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(o.stdout).unwrap().trim().to_string()
    }

    /// A second machine: a new kindex and project, with a clone of the repo at the same commit
    /// and no index yet.
    pub fn second_machine(s: &Setup) -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        git(
            s._tmp.path(),
            &[
                "clone",
                "-q",
                s.repo.to_str().unwrap(),
                repo.to_str().unwrap(),
            ],
        );
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        Setup {
            _tmp: tmp,
            kx,
            repo,
        }
    }

    pub fn graph_digest(s: &Setup) -> String {
        katalogr(s).audit_record(&opts()).unwrap().graph_digest
    }

    /// Replaces `from` with `to` in the first (header) lines of a snapshot, at equal or any
    /// length; the payload is untouched.
    fn edit_header(file: &Utf8Path, from: &str, to: &str) {
        let raw = std::fs::read(file).unwrap();
        let mut at = 0;
        for _ in 0..2 {
            at += raw[at..].iter().position(|b| *b == b'\n').unwrap() + 1;
        }
        let head = String::from_utf8(raw[..at].to_vec()).unwrap();
        assert!(head.contains(from), "{head}");
        let mut out = head.replace(from, to).into_bytes();
        out.extend_from_slice(&raw[at..]);
        std::fs::write(file, out).unwrap();
    }

    /// The extractor's call counter is process-wide, so the tests of this file run one at a time.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cache folder of the checkout layer a read from the repo uses.
    fn layer_cache(s: &Setup) -> Utf8PathBuf {
        let contexts = s.kx.join("projects/demo/.cache/contexts");
        let mut dirs: Vec<_> = std::fs::read_dir(&contexts)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        Utf8PathBuf::from_path_buf(dirs.pop().unwrap().join(".cache")).unwrap()
    }

    fn refused_exit(e: &DistError) -> Option<i32> {
        e.refused_exit()
    }

    #[test]
    fn export_writes_header_and_payload() {
        let _g = serial();
        let s = setup();
        let out = out_of(&s, "g.ksnap");
        let r = katalogr(&s).snapshot_export(&out, &opts()).unwrap();
        assert_eq!(r.schema, 1);
        let raw = std::fs::read(&out).unwrap();
        assert_eq!(r.file_bytes, raw.len() as u64);
        let mut lines = raw.splitn(3, |b| *b == b'\n');
        assert_eq!(lines.next().unwrap(), b"katalogr-snapshot 1");
        let m: SnapshotManifest = serde_json::from_slice(lines.next().unwrap()).unwrap();
        assert_eq!(m, r.manifest);
        assert_eq!(read_manifest(&out).unwrap(), m);
        assert_eq!(m.positions.len(), 1);
        assert_eq!(m.positions[0].repo, "app");
        assert_eq!(m.positions[0].commit, head(&s.repo));
        assert_eq!(m.cache_schema, katalogr_store::CACHE_SCHEMA);
        assert_eq!(m.facts_version.len(), 64);
        assert!(m.bytes > 0);
        // The payload is a frame that decodes to a database of the recorded size and digest.
        let target = out_of(&s, "decoded.redb");
        decode_snapshot(&out, &m, &target).unwrap();
        assert_eq!(std::fs::metadata(&target).unwrap().len(), m.bytes);
        // No temp file is left in the cache folder or beside the output.
        assert!(!s.kx.join("projects/demo/.cache/index.redb.export").exists());
        assert!(!out_of(&s, "g.ksnap.part").exists());
    }

    #[test]
    fn export_leaves_live_database_unchanged() {
        let _g = serial();
        let s = setup();
        let digest = |s: &Setup| katalogr(s).audit_record(&opts()).unwrap().graph_digest;
        let g0 = digest(&s);
        katalogr(&s)
            .snapshot_export(&out_of(&s, "g.ksnap"), &opts())
            .unwrap();
        assert_eq!(digest(&s), g0);
        // The index still answers and is still exact at the same commit.
        let r = katalogr(&s).find("beta", &opts()).unwrap();
        assert!(r.report.exact);
    }

    #[test]
    fn export_twice_is_byte_identical() {
        let _g = serial();
        let s = setup();
        let (a, b) = (out_of(&s, "a.ksnap"), out_of(&s, "b.ksnap"));
        katalogr(&s).snapshot_export(&a, &opts()).unwrap();
        katalogr(&s).snapshot_export(&b, &opts()).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    }

    #[test]
    fn export_dirty_refused() {
        let _g = serial();
        let s = setup();
        std::fs::write(s.repo.join("s.tx"), "sym fn alpha\nsym fn changed\n").unwrap();
        let out = out_of(&s, "g.ksnap");
        let e = katalogr(&s).snapshot_export(&out, &opts()).unwrap_err();
        assert_eq!(refused_exit(&e), Some(5), "{e}");
        assert!(e.to_string().contains("app"), "{e}");
        assert!(!out.exists());
    }

    #[test]
    fn export_no_commit_refused() {
        let _g = serial();
        let s = setup_with(false, None);
        let out = out_of(&s, "g.ksnap");
        let e = katalogr(&s).snapshot_export(&out, &opts()).unwrap_err();
        assert_eq!(refused_exit(&e), Some(5), "{e}");
        assert!(!out.exists());
    }

    #[test]
    fn export_undrained_refused() {
        let _g = serial();
        let s = setup();
        let k = katalogr(&s);
        // Another process holds the processor lock, so a queued event cannot be drained.
        let held = katalogr_engine::queue::lock::try_lock(&layer_cache(&s))
            .unwrap()
            .expect("lock");
        k.notify(&["s.tx".to_string()], false).unwrap();
        let out = out_of(&s, "g.ksnap");
        let o = Options {
            timeout: Some(0.5),
            ..Default::default()
        };
        let e = k.snapshot_export(&out, &o).unwrap_err();
        assert_eq!(refused_exit(&e), Some(5), "{e}");
        assert!(!out.exists());
        drop(held);
    }

    #[test]
    fn export_into_work_tree_refused() {
        let _g = serial();
        let s = setup();
        let out = Utf8PathBuf::from_path_buf(s.repo.join("g.ksnap")).unwrap();
        let e = katalogr(&s).snapshot_export(&out, &opts()).unwrap_err();
        assert!(matches!(e, DistError::Usage(_)), "{e}");
        assert!(e.is_usage());
        assert!(!out.exists());
    }

    #[test]
    fn export_to_ignored_path_allowed() {
        let _g = serial();
        let s = setup_with(true, Some("*.ksnap\n"));
        let out = Utf8PathBuf::from_path_buf(s.repo.join("g.ksnap")).unwrap();
        katalogr(&s).snapshot_export(&out, &opts()).unwrap();
        assert!(out.exists());
    }

    #[test]
    fn export_outside_git_allowed() {
        let _g = serial();
        let s = setup();
        let out = out_of(&s, "elsewhere/g.ksnap");
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        katalogr(&s).snapshot_export(&out, &opts()).unwrap();
        assert!(out.exists());
    }

    #[test]
    fn file_holds_no_absolute_path() {
        let _g = serial();
        let s = setup();
        let out = out_of(&s, "g.ksnap");
        let r = katalogr(&s).snapshot_export(&out, &opts()).unwrap();
        let target = out_of(&s, "decoded.redb");
        decode_snapshot(&out, &r.manifest, &target).unwrap();
        let bytes = std::fs::read(&target).unwrap();
        let root = s._tmp.path().to_str().unwrap().as_bytes();
        assert!(!bytes.windows(root.len()).any(|w| w == root));
        let header = std::fs::read(&out).unwrap();
        assert!(!header.windows(root.len()).any(|w| w == root));
        let _: &Utf8Path = &out;
    }

    /// Four more files, committed and indexed, so one changed file is a small part of the repo.
    fn grow(s: &Setup) {
        for i in 0..4 {
            std::fs::write(
                s.repo.join(format!("g{i}.tx")),
                format!("sym fn grown{i}\n"),
            )
            .unwrap();
        }
        commit_all(&s.repo, "grow");
        katalogr(s).index(None, true, None, &opts()).unwrap();
    }

    fn exported(s: &Setup) -> Utf8PathBuf {
        let out = out_of(s, "g.ksnap");
        katalogr(s).snapshot_export(&out, &opts()).unwrap();
        out
    }

    #[test]
    fn round_trip_graph_identical() {
        let _g = serial();
        let s = setup();
        let want = graph_digest(&s);
        let out = exported(&s);
        let t = second_machine(&s);
        let r = katalogr(&t)
            .snapshot_import(&out, false, None, &opts())
            .unwrap();
        assert!(r.facts_match, "{r:?}");
        assert!(r.rescans.is_empty() && r.no_root.is_empty());
        assert_eq!(graph_digest(&t), want);
    }

    #[test]
    fn audit_verify_matches_after_import() {
        let _g = serial();
        let s = setup();
        katalogr(&s).audit_record(&opts()).unwrap();
        let record = std::fs::read_dir(s.kx.join("projects/demo/audit"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let out = exported(&s);
        let t = second_machine(&s);
        katalogr(&t)
            .snapshot_import(&out, false, None, &opts())
            .unwrap();
        assert_eq!(
            katalogr(&t).audit_verify(Some(&record), &opts()).unwrap(),
            katalogr_dist::AuditOutcome::Match
        );
    }

    #[test]
    fn import_waits_for_processor_lock() {
        let _g = serial();
        let s = setup();
        let out = exported(&s);
        let t = second_machine(&s);
        let cache = Utf8PathBuf::from_path_buf(t.kx.join("projects/demo/.cache")).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        let held = katalogr_engine::queue::lock::try_lock(&cache)
            .unwrap()
            .expect("lock");
        // Held for the whole timeout: refused, and nothing installed.
        let short = Options {
            timeout: Some(0.3),
            ..Default::default()
        };
        let e = katalogr(&t)
            .snapshot_import(&out, false, None, &short)
            .unwrap_err();
        assert_eq!(e.refused_exit(), Some(5), "{e}");
        assert!(!cache.join("index.redb").exists());
        // Released during the wait: the import goes through.
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            drop(held);
        });
        let started = std::time::Instant::now();
        katalogr(&t)
            .snapshot_import(&out, false, None, &opts())
            .unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_millis(300));
        releaser.join().unwrap();
        assert!(cache.join("index.redb").exists());
    }

    #[test]
    fn flipped_byte_refused_database_kept() {
        let _g = serial();
        let s = setup();
        let out = exported(&s);
        let mut raw = std::fs::read(&out).unwrap();
        let mid = raw.len() / 2;
        raw[mid] ^= 0xff;
        std::fs::write(&out, raw).unwrap();
        let before = std::fs::read(cache_file(&s)).unwrap();
        let e = katalogr(&s)
            .snapshot_import(&out, true, None, &opts())
            .unwrap_err();
        assert!(
            matches!(e, DistError::Store(StoreError::BadSnapshot(_))),
            "{e}"
        );
        assert_eq!(std::fs::read(cache_file(&s)).unwrap(), before);
        assert!(!s.kx.join("projects/demo/.cache/index.redb.import").exists());
    }

    #[test]
    fn other_schema_refused() {
        let _g = serial();
        let s = setup();
        let out = exported(&s);
        let own = format!("\"cache_schema\":{}", katalogr_store::CACHE_SCHEMA);
        edit_header(&out, &own, "\"cache_schema\":999");
        let t = second_machine(&s);
        let e = katalogr(&t)
            .snapshot_import(&out, false, None, &opts())
            .unwrap_err();
        assert_eq!(e.refused_exit(), Some(5), "{e}");
        assert!(!t.kx.join("projects/demo/.cache/index.redb").exists());
    }

    #[test]
    fn existing_database_needs_replace() {
        let _g = serial();
        let s = setup();
        let out = exported(&s);
        let before = std::fs::read(cache_file(&s)).unwrap();
        let e = katalogr(&s)
            .snapshot_import(&out, false, None, &opts())
            .unwrap_err();
        assert_eq!(e.refused_exit(), Some(5), "{e}");
        assert_eq!(std::fs::read(cache_file(&s)).unwrap(), before);
    }

    #[test]
    fn replace_installs_over_existing() {
        let _g = serial();
        let s = setup();
        let want = graph_digest(&s);
        let out = exported(&s);
        // Another index is there: add a file and index it, so the databases differ.
        std::fs::write(s.repo.join("u.tx"), "sym fn delta\n").unwrap();
        commit_all(&s.repo, "c2");
        katalogr(&s).index(None, true, None, &opts()).unwrap();
        assert_ne!(graph_digest(&s), want);
        // The snapshot is at c1; installing it puts the c1 rows back under the c2 checkout,
        // so the check is on the installed rows, read at the snapshot's own position.
        let r = katalogr(&s)
            .snapshot_import(&out, true, None, &opts())
            .unwrap();
        assert_eq!(r.manifest.positions[0].repo, "app");
        let have = katalogr(&s).snapshot_show(&out, true).unwrap();
        assert_eq!(have, r.manifest);
        // The base now holds the c1 graph the snapshot was made from.
        let k = katalogr(&s);
        let base = katalogr_store::Cache::open_reader(k.project())
            .unwrap()
            .unwrap();
        let text = katalogr_engine::canonical_graph(&base.read().unwrap());
        use sha2::{Digest, Sha256};
        let have: String = Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(have, want);
    }

    #[test]
    fn unlisted_repo_refused() {
        let _g = serial();
        let s = setup();
        let out = exported(&s);
        edit_header(&out, "\"repo\":\"app\"", "\"repo\":\"other\"");
        let t = second_machine(&s);
        let e = katalogr(&t)
            .snapshot_import(&out, false, None, &opts())
            .unwrap_err();
        assert_eq!(e.refused_exit(), Some(5), "{e}");
        assert!(e.to_string().contains("other"), "{e}");
        assert!(!t.kx.join("projects/demo/.cache/index.redb").exists());
    }

    #[test]
    fn import_then_index_extracts_only_difference() {
        let _g = serial();
        let s = setup();
        grow(&s);
        let out = exported(&s);
        let t = second_machine(&s);
        // The new machine's clone is one commit ahead, with one file changed.
        std::fs::write(t.repo.join("t.tx"), "sym fn gamma\nsym fn gamma2\n").unwrap();
        commit_all(&t.repo, "c2");
        katalogr(&t)
            .snapshot_import(&out, false, None, &opts())
            .unwrap();
        let before = katalogr_dist::extract_calls();
        katalogr(&t).index(None, false, None, &opts()).unwrap();
        let extracted = katalogr_dist::extract_calls() - before;
        // The changed file, at most once in each of the two contexts; six files exist.
        assert!((1..=2).contains(&extracted), "extracted {extracted}");
        let hits = katalogr(&t).find("gamma2", &opts()).unwrap();
        assert!(matches!(hits.outcome, katalogr_dist::Outcome::Found(_)));
    }

    #[test]
    fn import_at_same_commit_extracts_nothing() {
        let _g = serial();
        let s = setup();
        let out = exported(&s);
        let t = second_machine(&s);
        katalogr(&t)
            .snapshot_import(&out, false, None, &opts())
            .unwrap();
        let before = katalogr_dist::extract_calls();
        katalogr(&t).index(None, false, None, &opts()).unwrap();
        assert_eq!(katalogr_dist::extract_calls() - before, 0);
    }

    #[test]
    fn missing_commit_falls_back_to_rescan() {
        let _g = serial();
        let s = setup();
        grow(&s);
        let out = exported(&s);
        // A clone that does not hold the snapshot's commit: a fresh history of its own.
        let t = second_machine(&s);
        std::fs::remove_dir_all(t.repo.join(".git")).unwrap();
        git(&t.repo, &["init", "-q"]);
        git(&t.repo, &["config", "user.name", "Test"]);
        git(&t.repo, &["config", "user.email", "test@example.com"]);
        git(&t.repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(t.repo.join("t.tx"), "sym fn gamma\nsym fn gamma2\n").unwrap();
        commit_all(&t.repo, "other history");
        katalogr(&t)
            .snapshot_import(&out, false, None, &opts())
            .unwrap();
        let before = katalogr_dist::extract_calls();
        katalogr(&t).index(None, false, None, &opts()).unwrap();
        // The rescan compares file revisions: only the changed file is extracted.
        let extracted = katalogr_dist::extract_calls() - before;
        assert!((1..=2).contains(&extracted), "extracted {extracted}");
        let hits = katalogr(&t).find("gamma2", &opts()).unwrap();
        assert!(matches!(hits.outcome, katalogr_dist::Outcome::Found(_)));
    }

    #[test]
    fn import_seeds_checkout_context() {
        let _g = serial();
        let s = setup();
        let out = exported(&s);
        let t = second_machine(&s);
        katalogr(&t)
            .snapshot_import(&out, false, None, &opts())
            .unwrap();
        // The first read from the checkout creates its layer from the imported base.
        let r = katalogr(&t).find("alpha", &opts()).unwrap();
        assert!(matches!(r.outcome, katalogr_dist::Outcome::Found(_)));
        assert!(r.report.exact, "{:?}", r.report);
        assert!(t.kx.join("projects/demo/.cache/contexts").is_dir());
        assert_eq!(
            r.report.positions[0].commit.as_deref(),
            Some(head(&t.repo).as_str())
        );
    }

    #[test]
    fn other_facts_queues_rescan() {
        let _g = serial();
        let s = setup();
        let out = exported(&s);
        let other = "f".repeat(64);
        let m = read_manifest(&out).unwrap();
        edit_header(&out, &m.facts_version, &other);
        let t = second_machine(&s);
        let r = katalogr(&t)
            .snapshot_import(&out, false, None, &opts())
            .unwrap();
        assert!(!r.facts_match);
        assert_eq!(r.rescans, ["app"]);
        let inbox = t.kx.join("projects/demo/.cache/inbox");
        assert_eq!(std::fs::read_dir(inbox).unwrap().count(), 1);
    }

    #[test]
    fn other_facts_warns_for_rootless_repo() {
        let _g = serial();
        let s = setup();
        let out = exported(&s);
        let m = read_manifest(&out).unwrap();
        edit_header(&out, &m.facts_version, &"f".repeat(64));
        let t = second_machine(&s);
        // The project lists a repo with no root here.
        let cfg = std::fs::read_to_string(t.kx.join("projects/demo/config.yaml")).unwrap();
        assert!(cfg.contains("app"));
        let mut roots = std::fs::read_to_string(t.kx.join("projects/demo/.cache/roots.yaml"))
            .unwrap_or_default();
        roots.clear();
        std::fs::write(t.kx.join("projects/demo/.cache/roots.yaml"), roots).unwrap();
        let k = Katalogr::resolve(
            std::path::Path::new("/"),
            &Overrides {
                kindex: Some(t.kx.display().to_string()),
                project: Some("demo".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let r = k.snapshot_import(&out, false, None, &opts()).unwrap();
        assert!(!r.facts_match);
        assert!(r.rescans.is_empty(), "{r:?}");
        assert_eq!(r.no_root, ["app"]);
        assert!(r.warnings.iter().any(|w| w.contains("app")), "{r:?}");
    }

    #[test]
    fn pending_and_events_are_empty_after_import() {
        let _g = serial();
        let s = setup();
        // A queued event is in the source index when it is exported: it must not travel.
        let out = exported(&s);
        let t = second_machine(&s);
        katalogr(&t)
            .snapshot_import(&out, false, None, &opts())
            .unwrap();
        use katalogr_store::Reader;
        let k = katalogr(&t);
        let cache = katalogr_store::Cache::open_reader(k.project())
            .unwrap()
            .unwrap();
        let tx = cache.read().unwrap();
        assert!(tx.pending_entries(None).unwrap().is_empty());
        assert_eq!(tx.event_count("worktree").unwrap(), 0);
    }

    /// A machine with the project but no clone of the repo: the snapshot imported, no root.
    fn no_clone(s: &Setup) -> (Setup, Katalogr) {
        let t = second_machine(s);
        let out = exported(s);
        katalogr(&t)
            .snapshot_import(&out, false, None, &opts())
            .unwrap();
        std::fs::remove_file(t.kx.join("projects/demo/.cache/roots.yaml")).ok();
        let k = rootless(&t);
        (t, k)
    }

    fn rootless(t: &Setup) -> Katalogr {
        Katalogr::resolve(
            std::path::Path::new("/"),
            &Overrides {
                kindex: Some(t.kx.display().to_string()),
                project: Some("demo".into()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn no_clone_answers_find() {
        let _g = serial();
        let s = setup();
        let (_t, k) = no_clone(&s);
        let r = k.find("alpha", &opts()).unwrap();
        assert!(
            matches!(&r.outcome, katalogr_dist::Outcome::Found(h) if !h.is_empty()),
            "{:?}",
            r.report
        );
        assert!(r.report.exact, "{:?}", r.report);
        assert!(!r.report.stale);
    }

    #[test]
    fn no_clone_reports_positions_and_warning() {
        let _g = serial();
        let s = setup();
        let commit = head(&s.repo);
        let (_t, k) = no_clone(&s);
        let r = k.find("alpha", &opts()).unwrap();
        assert_eq!(r.report.positions.len(), 1);
        assert_eq!(r.report.positions[0].repo, "app");
        assert_eq!(
            r.report.positions[0].commit.as_deref(),
            Some(commit.as_str())
        );
        assert_eq!(r.report.positions[0].dirty, None);
        let want = format!(
            "repo app has no checkout here: answering from its imported index at {}",
            &commit[..12]
        );
        assert!(r.report.warnings.contains(&want), "{:?}", r.report.warnings);
    }

    #[test]
    fn no_clone_view_id_matches_source_machine() {
        let _g = serial();
        let s = setup();
        let view = |k: &Katalogr| {
            let o = Options {
                view: Some(katalogr_dist::ViewQuery {
                    op: "find".into(),
                    args: std::collections::BTreeMap::from([("text".into(), "alpha".into())]),
                }),
                ..opts()
            };
            k.find("alpha", &o).unwrap().report.view_id
        };
        let source = view(&katalogr(&s));
        assert!(source.is_some());
        let (_t, k) = no_clone(&s);
        assert_eq!(view(&k), source);
    }

    #[test]
    fn no_clone_show_says_no_checkout() {
        let _g = serial();
        let s = setup();
        let (_t, k) = no_clone(&s);
        let r = k.show("alpha", false, &opts()).unwrap();
        match r.outcome {
            katalogr_dist::Outcome::NoMatch(m) => {
                assert_eq!(m, "repo app has no checkout here");
            }
            other => panic!("expected NoMatch, got {other:?}"),
        }
    }

    struct Two {
        _tmp: tempfile::TempDir,
        kx: PathBuf,
        app: PathBuf,
        other: PathBuf,
    }

    /// Two repos in one project, `app` and `other`, each with a `.tx` file. `other_commits`:
    /// whether `other` has a commit (else it is a git repo with none).
    fn two(other_commits: bool) -> Two {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (kx, app, other) = (root.join("kx"), root.join("app"), root.join("other"));
        for (dir, text, commits) in [
            (&app, "sym fn appfn\n", true),
            (&other, "sym fn otherfn\n", other_commits),
        ] {
            std::fs::create_dir_all(dir).unwrap();
            git(dir, &["init", "-q", "-b", "main"]);
            git(dir, &["config", "user.name", "Test"]);
            git(dir, &["config", "user.email", "test@example.com"]);
            git(dir, &["config", "commit.gpgsign", "false"]);
            std::fs::write(dir.join("a.tx"), text).unwrap();
            std::fs::write(dir.join(".gitignore"), ".katalogr\n").unwrap();
            if commits {
                commit_all(dir, "c1");
            }
        }
        kindex_init(&kx).unwrap();
        project_init(
            &kx,
            "demo",
            vec![("app".into(), app.clone()), ("other".into(), other.clone())],
            true,
        )
        .unwrap();
        Two {
            _tmp: tmp,
            kx,
            app,
            other,
        }
    }

    fn two_k(t: &Two, context: Option<&str>) -> Katalogr {
        Katalogr::resolve(
            &t.app,
            &Overrides {
                kindex: Some(t.kx.display().to_string()),
                context: context.map(str::to_string),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn two_out(t: &Two, name: &str) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(t._tmp.path().join(name)).unwrap()
    }

    /// FEAT-117 R-1: a checkout context records the other repo at its pin.
    #[test]
    fn two_repo_export_in_checkout_context() {
        let _g = serial();
        let t = two(true);
        two_k(&t, None).index(None, false, None, &opts()).unwrap();
        let r = two_k(&t, None)
            .snapshot_export(&two_out(&t, "g.ksnap"), &opts())
            .unwrap();
        let got: Vec<(String, String)> = r
            .manifest
            .positions
            .iter()
            .map(|p| (p.repo.clone(), p.commit.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("app".to_string(), head(&t.app)),
                ("other".to_string(), head(&t.other))
            ]
        );
    }

    /// FEAT-117 R-1: the base pins every git repo, and exports them all.
    #[test]
    fn export_in_base_context_pinned_repos() {
        let _g = serial();
        let t = two(true);
        two_k(&t, None).index(None, false, None, &opts()).unwrap();
        let r = two_k(&t, Some("base"))
            .snapshot_export(&two_out(&t, "b.ksnap"), &opts())
            .unwrap();
        assert_eq!(r.manifest.positions.len(), 2);
        assert!(r.manifest.positions.iter().all(|p| !p.commit.is_empty()));
    }

    /// FEAT-117 R-2: a pinned repo with no commit is refused by name, with the command.
    #[test]
    fn export_pin_without_commit_refused() {
        let _g = serial();
        let t = two(false);
        two_k(&t, None).index(None, false, None, &opts()).unwrap();
        let out = two_out(&t, "n.ksnap");
        let e = two_k(&t, None).snapshot_export(&out, &opts()).unwrap_err();
        assert_eq!(refused_exit(&e), Some(5), "{e}");
        assert!(e.to_string().contains("other"), "{e}");
        assert!(e.to_string().contains("pinned with no commit"), "{e}");
        assert!(e.to_string().contains("katalogr index"), "{e}");
        assert!(!out.exists());
    }

    /// FEAT-117 R-3: a single-repo project exports as before.
    #[test]
    fn single_repo_export_unchanged() {
        let _g = serial();
        let s = setup();
        let r = katalogr(&s)
            .snapshot_export(&out_of(&s, "one.ksnap"), &opts())
            .unwrap();
        assert_eq!(r.manifest.positions.len(), 1);
        assert_eq!(r.manifest.positions[0].commit, head(&s.repo));
    }
}
