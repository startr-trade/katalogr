//! FEAT-088 T1 and T3: the view store on disk. Needs the `.tx` test extractor for the tests
//! that read a project: `cargo test -p katalogr-dist --features test-extractor`.

mod view_store {
    use katalogr_dist::{Katalogr, Options, Overrides, ViewStore, kindex_init, project_init};
    use std::path::Path;

    /// An empty project `demo` with one repo `app`; its resolved handle.
    fn project(tmp: &Path) -> Katalogr {
        let kx = tmp.join("kx");
        kindex_init(&kx).unwrap();
        let dir = tmp.join("app");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.tx"), "sym fn a\n").unwrap();
        project_init(&kx, "demo", vec![("app".to_string(), dir.clone())], true).unwrap();
        Katalogr::resolve(
            &dir,
            &Overrides {
                kindex: Some(kx.display().to_string()),
                project: None,
                context: None,
            },
        )
        .unwrap()
    }

    fn id(n: u128) -> String {
        format!("v1-{n:032x}")
    }

    fn body(id: &str, pad: usize) -> Vec<u8> {
        format!("{{\"view_id\":\"{id}\",\"pad\":\"{}\"}}", "x".repeat(pad)).into_bytes()
    }

    fn files(store_dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for bucket in std::fs::read_dir(store_dir).into_iter().flatten().flatten() {
            for f in std::fs::read_dir(bucket.path()).unwrap().flatten() {
                out.push(f.file_name().to_string_lossy().to_string());
            }
        }
        out.sort();
        out
    }

    /// R-9: a file that does not parse, or holds another ID, is a miss; the next put heals it.
    #[test]
    fn corrupt_entry_is_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path());
        let store = ViewStore::open(k.project(), u64::MAX);
        let a = id(1);
        assert_eq!(store.get(&a), None, "absent");
        store.put(&a, &body(&a, 8)).unwrap();
        assert_eq!(store.get(&a), Some(body(&a, 8)));
        let path = k
            .project()
            .cache_dir()
            .as_std_path()
            .join("views")
            .join(&a[3..5])
            .join(format!("{a}.json"));
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(store.get(&a), None, "unparsable");
        std::fs::write(&path, body(&id(2), 8)).unwrap();
        assert_eq!(store.get(&a), None, "another id");
        store.put(&a, &body(&a, 8)).unwrap();
        assert_eq!(store.get(&a), Some(body(&a, 8)), "replaced");
        assert_eq!(store.get("../escape"), None);
        assert!(store.put("../escape", b"{}").is_err());
    }

    /// R-9: many writers of one ID leave one file, whole, and no temp files.
    #[test]
    fn concurrent_puts_keep_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path());
        let store = ViewStore::open(k.project(), u64::MAX);
        let a = id(7);
        let bytes = body(&a, 4096);
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    for _ in 0..10 {
                        store.put(&a, &bytes).unwrap();
                        if let Some(got) = store.get(&a) {
                            assert_eq!(got, bytes, "a reader saw a partial file");
                        }
                    }
                });
            }
        });
        assert_eq!(store.get(&a), Some(bytes));
        let dir = k.project().cache_dir().as_std_path().join("views");
        assert_eq!(files(&dir), [format!("{a}.json")]);
    }

    /// R-10: prune drops the oldest entries until the folder is within the cap.
    #[test]
    fn prune_keeps_newest_within_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path());
        let big = ViewStore::open(k.project(), u64::MAX);
        let mut size = 0;
        for n in 1..=5 {
            let i = id(n);
            let b = body(&i, 200);
            size = b.len() as u64;
            big.put(&i, &b).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let small = ViewStore::open(k.project(), size * 3);
        assert_eq!(small.prune().unwrap(), 2);
        for n in 1..=2 {
            assert_eq!(small.get(&id(n)), None, "{n} is oldest");
        }
        for n in 3..=5 {
            assert!(small.get(&id(n)).is_some(), "{n} is kept");
        }
        assert_eq!(small.prune().unwrap(), 0);
        let none = ViewStore::open(k.project(), 0);
        assert_eq!(none.prune().unwrap(), 3);
    }

    /// R-9: a store that cannot be written never fails a read; the answer carries a warning.
    #[cfg(feature = "test-extractor")]
    #[test]
    fn write_failure_still_answers() {
        use katalogr_dist::Outcome;
        use katalogr_dist::render::ViewSpec;
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path());
        k.index(None, true, None, &Options::default()).unwrap();
        // A file where the store's folder should be.
        let cache = k.project().cache_dir().as_std_path().to_path_buf();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("views"), b"in the way").unwrap();
        let spec = ViewSpec::parse("structure --level file").unwrap();
        let r = k.view(&spec, &Options::default()).unwrap();
        assert!(matches!(r.outcome, Outcome::Found(_)), "{:?}", r.outcome);
        assert!(
            r.report.warnings.iter().any(|w| w.contains("not stored")),
            "{:?}",
            r.report.warnings
        );
    }
}
