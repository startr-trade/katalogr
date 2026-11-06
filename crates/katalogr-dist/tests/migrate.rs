//! FEAT-094 R-15 through the facade: a format 1 project, with a `shards/` folder left by the
//! previous release, is never read; it rebuilds to the same answers as a new project. Needs the
//! `.tx` test extractor: `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod migrate {
    use katalogr_dist::{Katalogr, Options, Overrides, kindex_init, project_init};
    use katalogr_store::{CACHE_SCHEMA, Cache, Kindex, ProjectName};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    struct Setup {
        _tmp: tempfile::TempDir,
        kx: PathBuf,
        repo: PathBuf,
    }

    fn setup() -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("s.tx"), "sym fn alpha\nsym fn beta\n").unwrap();
        std::fs::write(repo.join("t.tx"), "sym fn gamma\nref gamma calls alpha\n").unwrap();
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        let s = Setup {
            _tmp: tmp,
            kx,
            repo,
        };
        katalogr(&s)
            .index(None, true, None, &Options::default())
            .unwrap();
        s
    }

    fn overrides(kx: &Path) -> Overrides {
        Overrides {
            kindex: Some(kx.display().to_string()),
            project: None,
            ..Default::default()
        }
    }

    fn katalogr(s: &Setup) -> Katalogr {
        Katalogr::resolve(&s.repo, &overrides(&s.kx)).unwrap()
    }

    fn project_root(s: &Setup) -> camino::Utf8PathBuf {
        Kindex::open(camino::Utf8Path::from_path(&s.kx).unwrap())
            .unwrap()
            .project(&ProjectName::new("demo").unwrap())
            .unwrap()
            .project_root()
            .to_owned()
    }

    fn find_outcome(s: &Setup, text: &str) -> String {
        format!(
            "{:?}",
            katalogr(s).find(text, &Options::default()).unwrap().outcome
        )
    }

    /// R-15: a `shards/` folder from the previous release is ignored; its bytes are left alone
    /// and the answers come from the graph.
    #[test]
    fn leftover_shards_are_ignored() {
        let s = setup();
        let before = find_outcome(&s, "alpha");
        let leftover = project_root(&s).join("shards/app/s.tx.kshard.json");
        std::fs::create_dir_all(leftover.parent().unwrap()).unwrap();
        std::fs::write(&leftover, b"not a shard at all").unwrap();
        katalogr(&s)
            .index(None, true, None, &Options::default())
            .unwrap();
        assert_eq!(find_outcome(&s, "alpha"), before);
        assert_eq!(std::fs::read(&leftover).unwrap(), b"not a shard at all");
    }

    /// R-15: a format 1 project with a `shards/` folder and a schema-mismatched cache rebuilds
    /// to the same answers as a new project, and its format file stays at 1.
    #[test]
    fn old_project_rebuilds_identical() {
        let fresh = setup();
        let want = find_outcome(&fresh, "alpha");

        let old = setup();
        let root = project_root(&old);
        std::fs::write(
            root.join("format.json"),
            "{\n  \"min_katalogr\": \"0.1.0\",\n  \"shard_format\": 1\n}\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("shards/app")).unwrap();
        std::fs::write(root.join("shards/app/s.tx.kshard.json"), b"{}").unwrap();
        {
            let dir = Kindex::open(camino::Utf8Path::from_path(&old.kx).unwrap())
                .unwrap()
                .project(&ProjectName::new("demo").unwrap())
                .unwrap();
            let c = Cache::open(&dir, Instant::now() + Duration::from_secs(5)).unwrap();
            let mut tx = c.write().unwrap();
            tx.set_meta("cache_schema", &(CACHE_SCHEMA + 1).to_le_bytes())
                .unwrap();
            tx.commit().unwrap();
        }
        katalogr(&old)
            .index(None, true, None, &Options::default())
            .unwrap();
        assert_eq!(find_outcome(&old, "alpha"), want);
        let format = std::fs::read_to_string(root.join("format.json")).unwrap();
        assert!(format.contains("\"shard_format\": 1"), "{format}");
    }
}
