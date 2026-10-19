//! FEAT-064 T1: read-only cache inspection.

mod inspect {
    use camino::Utf8PathBuf;
    use katalogr_model::ArtifactName;
    use katalogr_store::{
        CACHE_SCHEMA, Cache, CacheInspection, Kindex, ProjectConfig, ProjectDir, ProjectName,
        Restitch, StoreError, WriteTx,
    };
    use std::time::{Duration, Instant};

    struct Noop;

    impl Restitch for Noop {
        fn replace_fragment(
            &self,
            _: &mut WriteTx,
            _: katalogr_model::Fragment,
        ) -> Result<(), StoreError> {
            Ok(())
        }
        fn remove_artifact(&self, _: &mut WriteTx, _: &ArtifactName) -> Result<(), StoreError> {
            Ok(())
        }
    }

    fn project(t: &tempfile::TempDir) -> ProjectDir {
        let root = Utf8PathBuf::from_path_buf(t.path().join("kx")).unwrap();
        Kindex::init(&root)
            .unwrap()
            .create_project(&ProjectName::new("p").unwrap(), ProjectConfig::default())
            .unwrap()
    }

    fn open(dir: &ProjectDir) -> Cache {
        Cache::open_or_rebuild(dir, &Noop, Instant::now() + Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn missing_inuse_unreadable_open() {
        let t = tempfile::tempdir().unwrap();
        let dir = project(&t);
        let file = dir.cache_dir().join("index.redb");

        // Missing: nothing is created.
        assert!(matches!(
            Cache::inspect(&dir).unwrap(),
            CacheInspection::Missing
        ));
        assert!(!file.exists());

        // InUse: a writer holds the database.
        let cache = open(&dir);
        assert!(matches!(
            Cache::inspect(&dir).unwrap(),
            CacheInspection::InUse
        ));

        // A schema other than the current one is reported without reading the tables.
        let mut tx = cache.write().unwrap();
        tx.set_meta("cache_schema", &(CACHE_SCHEMA + 1).to_le_bytes())
            .unwrap();
        tx.commit().unwrap();
        drop(cache);
        match Cache::inspect(&dir).unwrap() {
            CacheInspection::Open(o) => {
                assert_eq!(o.stored_schema, Some(CACHE_SCHEMA + 1));
                assert!(o.tables.is_none());
            }
            other => panic!("{other:?}"),
        }

        // Open: current schema, tables read.
        std::fs::remove_file(&file).unwrap();
        let cache = open(&dir);
        let mut tx = cache.write().unwrap();
        tx.set_publish_journal(&[("a".to_string(), "b".to_string())])
            .unwrap();
        tx.commit().unwrap();
        drop(cache);
        match Cache::inspect(&dir).unwrap() {
            CacheInspection::Open(o) => {
                assert_eq!(o.stored_schema, Some(CACHE_SCHEMA));
                let tables = o.tables.unwrap();
                assert_eq!(tables.journal, vec![("a".to_string(), "b".to_string())]);
                assert!(tables.pending.is_empty());
                assert!(tables.shard_hashes.is_empty());
            }
            other => panic!("{other:?}"),
        }

        // Unreadable: a truncated file; the message names no path.
        let bytes = std::fs::read(&file).unwrap();
        std::fs::write(&file, &bytes[..100]).unwrap();
        match Cache::inspect(&dir).unwrap() {
            CacheInspection::Unreadable(m) => {
                assert!(!m.contains(t.path().to_str().unwrap()), "{m}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn does_not_write() {
        let t = tempfile::tempdir().unwrap();
        let dir = project(&t);
        drop(open(&dir));
        let file = dir.cache_dir().join("index.redb");
        let before = std::fs::read(&file).unwrap();
        let listing = |d: &ProjectDir| {
            let mut v: Vec<_> = std::fs::read_dir(d.cache_dir())
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            v.sort();
            v
        };
        let names = listing(&dir);
        for _ in 0..2 {
            assert!(matches!(
                Cache::inspect(&dir).unwrap(),
                CacheInspection::Open(_)
            ));
        }
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert_eq!(listing(&dir), names);
    }
}
