mod support;

use camino::Utf8PathBuf;
use katalogr_engine::Stitcher;
use katalogr_model::{ExtractInput, Extractor, Fragment, RelPath};
use katalogr_store::{Cache, Kindex, ProjectConfig, ProjectDir, ProjectName, Reader, RepoConfig};
use std::time::{Duration, Instant};
use support::TestExtractor;

fn project(tmp: &tempfile::TempDir) -> ProjectDir {
    let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
    Kindex::init(&root)
        .unwrap()
        .create_project(
            &ProjectName::new("p").unwrap(),
            ProjectConfig {
                repos: vec![RepoConfig::new("r")],
                ..Default::default()
            },
        )
        .unwrap()
}

fn extract(path: &str, text: &str) -> Fragment {
    let path = RelPath::new(path).unwrap();
    TestExtractor
        .extract(&ExtractInput {
            repo: "r",
            path: &path,
            revision: "ce013625030ba8dba906f756967f9e9ca394464a",
            content: text.as_bytes(),
            context: &[],
        })
        .unwrap()
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

mod kindex {
    use super::*;

    #[test]
    fn cache_rebuilds_from_shards() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        for (p, t) in [
            ("a.tx", "sym fn A\nexport ns:k A\n"),
            (
                "b.tx",
                "sym fn B\nsym fn B2\nlink B calls B2\nref B calls ns:k\n",
            ),
            ("c.tx", "sym fn C\nref C calls ns:nope\n"),
        ] {
            dir.write_shard(&extract(p, t)).unwrap();
        }

        let stitcher = Stitcher::new();
        let cache = Cache::open_or_rebuild(&dir, &stitcher, soon()).unwrap();
        assert_eq!(cache.report().replaced, 3);
        let before = cache.read().unwrap().dump_graph().unwrap();
        let rows = |name: &str| {
            before
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, r)| r.len())
                .unwrap()
        };
        assert_eq!(rows("artifacts"), 3);
        assert_eq!(rows("symbols"), 4);
        assert_eq!(rows("links_out"), 2);
        assert_eq!(rows("refs_unresolved"), 1);
        drop(cache);

        // Warm open: nothing changed, nothing replayed.
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().replaced, 0);
        assert_eq!(cache.read().unwrap().dump_graph().unwrap(), before);
        drop(cache);

        // Delete the cache; rebuilding from the shards gives the same graph.
        std::fs::remove_dir_all(dir.cache_dir()).unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().replaced, 3);
        assert_eq!(cache.read().unwrap().dump_graph().unwrap(), before);
        drop(cache);

        // A shard removed on disk is removed from the cache; same result as a fresh build.
        std::fs::remove_file(dir.shard_path("r", &RelPath::new("a.tx").unwrap())).unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().removed, 1);
        let incremental = cache.read().unwrap().dump_graph().unwrap();
        drop(cache);
        std::fs::remove_dir_all(dir.cache_dir()).unwrap();
        let fresh = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(fresh.read().unwrap().dump_graph().unwrap(), incremental);
        assert_ne!(incremental, before);
    }
}

mod cache {
    use super::*;
    use katalogr_store::StoreError;

    #[test]
    fn warm_open_skips_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        for (p, t) in [("a.tx", "sym fn A\n"), ("b.tx", "sym fn B\n")] {
            dir.write_shard(&extract(p, t)).unwrap();
        }
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().hashed, 2);
        drop(cache);

        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().hashed, 0);
        assert_eq!(cache.report().replaced, 0);
        drop(cache);

        dir.write_shard(&extract("a.tx", "sym fn A\nsym fn A2\n"))
            .unwrap();
        let cache = Cache::open_or_rebuild(&dir, &Stitcher::new(), soon()).unwrap();
        assert_eq!(cache.report().hashed, 1);
        assert_eq!(cache.report().replaced, 1);
    }

    #[test]
    fn busy_retries_until_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(&tmp);
        let stitcher = Stitcher::new();
        let _held = Cache::open_or_rebuild(&dir, &stitcher, soon()).unwrap();

        let start = Instant::now();
        let second = Cache::open_or_rebuild(&dir, &stitcher, start + Duration::from_millis(200));
        let waited = start.elapsed();
        assert!(matches!(second, Err(StoreError::Busy)));
        assert!(
            waited >= Duration::from_millis(200),
            "returned after {waited:?}"
        );
        assert!(waited < Duration::from_secs(3), "returned after {waited:?}");
    }
}
