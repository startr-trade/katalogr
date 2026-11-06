//! FEAT-097 R-19: an install killed at either side of the rename. The test re-runs its own
//! binary as a child with `KATALOGR_FAILPOINT` set; the child aborts and the parent checks the
//! database. Run with `--features failpoints`.
#![cfg(feature = "failpoints")]

mod snapshot_crash {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_store::{
        CACHE_SCHEMA, Cache, IMPORT_FILE, Kindex, ProjectConfig, ProjectDir, ProjectName, Reader,
        RepoConfig, SnapshotManifest, install_snapshot, read_manifest, write_snapshot,
    };
    use sha2::{Digest, Sha256};
    use std::process::Command;
    use std::time::{Duration, Instant};

    const CHILD_ENV: &str = "KATALOGR_SNAPSHOT_CRASH_CHILD";

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    /// A project whose database holds `meta.which = which`.
    fn project(root: &Utf8Path, name: &str, which: &str) -> ProjectDir {
        let kx = Kindex::init(&root.join(format!("kx-{name}"))).unwrap();
        let dir = kx
            .create_project(
                &ProjectName::new("p").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let cache = Cache::open(&dir, deadline()).unwrap();
        let mut tx = cache.write().unwrap();
        tx.set_meta("which", which.as_bytes()).unwrap();
        tx.commit().unwrap();
        dir
    }

    fn which(dir: &ProjectDir) -> String {
        let cache = Cache::open(dir, deadline()).unwrap();
        let v = cache.read().unwrap().meta("which").unwrap().unwrap();
        String::from_utf8(v).unwrap()
    }

    fn digest(path: &Utf8Path) -> Vec<u8> {
        Sha256::digest(std::fs::read(path).unwrap()).to_vec()
    }

    struct Scenario {
        _tmp: tempfile::TempDir,
        old: ProjectDir,
        snapshot: Utf8PathBuf,
    }

    /// An "old" database in place, and a snapshot of a "new" one.
    fn scenario() -> Scenario {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let old = project(&root, "old", "old");
        let new = project(&root, "new", "new");
        let snapshot = root.join("new.ksnap");
        let manifest = SnapshotManifest {
            format: 1,
            bytes: 0,
            positions: Vec::new(),
            facts_version: "f".repeat(64),
            cache_schema: CACHE_SCHEMA,
            digest: String::new(),
        };
        write_snapshot(&new.cache_dir().join("index.redb"), manifest, &snapshot).unwrap();
        Scenario {
            _tmp: tmp,
            old,
            snapshot,
        }
    }

    fn run_child(s: &Scenario, failpoint: &str) {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "snapshot_crash::crash_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, format!("{}|{}", s.snapshot, s.old.cache_dir()))
            .env("KATALOGR_FAILPOINT", failpoint)
            .status()
            .unwrap();
        assert!(!status.success(), "child survived failpoint {failpoint}");
    }

    /// Not a test of its own: the child half. It does nothing unless the parent set the variable.
    #[test]
    fn crash_child() {
        let Ok(spec) = std::env::var(CHILD_ENV) else {
            return;
        };
        let (file, cache) = spec.split_once('|').unwrap();
        let file = Utf8Path::new(file);
        let manifest = read_manifest(file).unwrap();
        install_snapshot(file, &manifest, Utf8Path::new(cache)).unwrap();
    }

    #[test]
    fn killed_before_rename_keeps_old() {
        let s = scenario();
        let live = s.old.cache_dir().join("index.redb");
        let before = digest(&live);
        run_child(&s, "snapshot_before_rename");
        assert_eq!(digest(&live), before, "the old database is untouched");
        assert_eq!(which(&s.old), "old");
        // The decoded copy was left behind, complete but not installed.
        assert!(s.old.cache_dir().join(IMPORT_FILE).exists());
    }

    #[test]
    fn killed_after_rename_has_new() {
        let s = scenario();
        run_child(&s, "snapshot_after_rename");
        assert_eq!(which(&s.old), "new");
        assert!(!s.old.cache_dir().join(IMPORT_FILE).exists());
    }

    #[test]
    fn leftover_temp_removed() {
        let s = scenario();
        run_child(&s, "snapshot_before_rename");
        let tmp = s.old.cache_dir().join(IMPORT_FILE);
        assert!(tmp.exists());
        // Plant junk over the leftover: the next import must not trust it.
        std::fs::write(&tmp, b"junk from a dead process").unwrap();
        let manifest = read_manifest(&s.snapshot).unwrap();
        install_snapshot(&s.snapshot, &manifest, &s.old.cache_dir()).unwrap();
        assert!(!tmp.exists());
        assert_eq!(which(&s.old), "new");
    }
}
