//! FEAT-097: graph snapshot export and import through the facade. Needs the `.tx` test
//! extractor: `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod snapshot {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_dist::{DistError, Katalogr, Options, Overrides, kindex_init, project_init};
    use katalogr_store::{SnapshotManifest, decode_snapshot, read_manifest};
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
}
