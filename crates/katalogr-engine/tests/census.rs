mod support;

use camino::Utf8PathBuf;
use katalogr_engine::census::census;
use katalogr_engine::registry::ExtensionRegistry;
use support::{TestExtractor, init_project, open_engine_with};

mod census {
    use super::*;

    #[test]
    fn unclaimed_files_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join("target")).unwrap();
        std::fs::create_dir_all(work.join("src")).unwrap();
        std::fs::write(work.join("src/a.tx"), "sym fn A\n").unwrap();
        std::fs::write(work.join("src/lib.rs"), "").unwrap();
        std::fs::write(work.join("logo.svg"), "").unwrap();
        std::fs::write(work.join("target/x.rs"), "").unwrap();
        let kx = root.join("kx");
        init_project(&kx);
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(TestExtractor));
        let engine = open_engine_with(&kx, &work, reg);

        let c = census(&engine, "r").unwrap().unwrap();
        let claimed: Vec<_> = c.claimed.iter().map(|(p, e)| (p.as_str(), *e)).collect();
        assert_eq!(claimed.len(), 1, "{claimed:?}");
        assert_eq!(claimed[0].0, "src/a.tx");
        let unclaimed: Vec<_> = c.unclaimed.iter().map(|p| p.as_str()).collect();
        assert_eq!(unclaimed, ["logo.svg", "src/lib.rs"]);

        // A repo with no root on this machine is not walked.
        assert!(census(&engine, "elsewhere").unwrap().is_none());
    }

    /// A project with the profile `routes.yaml` routing `data/*.dat` to the test extension.
    fn routed_engine(tmp: &tempfile::TempDir) -> katalogr_engine::Engine {
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join("data")).unwrap();
        std::fs::create_dir_all(work.join("src")).unwrap();
        std::fs::write(work.join("data/a.dat"), "sym fn A\n").unwrap();
        std::fs::write(work.join("src/b.tx"), "sym fn B\n").unwrap();
        std::fs::write(work.join("plain.txt"), "hello\n").unwrap();
        let kx = root.join("kx");
        init_project(&kx);
        let pdir = kx.join("projects/p/profiles");
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(
            pdir.join("routes.yaml"),
            "profile: routes\nversion: 2\nselect:\n  - extension: test\n    path: 'data/*.dat'\nrules: []\n",
        )
        .unwrap();
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(TestExtractor));
        open_engine_with(&kx, &work, reg)
    }

    #[test]
    fn select_routed_file_claimed() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = routed_engine(&tmp);
        let c = census(&engine, "r").unwrap().unwrap();
        let claimed: Vec<_> = c.claimed.iter().map(|(p, e)| (p.as_str(), *e)).collect();
        assert_eq!(claimed, [("data/a.dat", "test"), ("src/b.tx", "test")]);
        let unclaimed: Vec<_> = c.unclaimed.iter().map(|p| p.as_str()).collect();
        assert_eq!(unclaimed, ["plain.txt"]);
    }

    #[test]
    fn routing_matches_indexer() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = routed_engine(&tmp);
        let c = census(&engine, "r").unwrap().unwrap();
        support::full_index(&engine);
        let has_shard =
            |p: &katalogr_model::RelPath| engine.dir().shard_path("r", p).as_std_path().exists();
        assert!(!c.claimed.is_empty());
        for (p, _) in &c.claimed {
            assert!(has_shard(p), "claimed {p} has no shard");
        }
        for p in &c.unclaimed {
            assert!(!has_shard(p), "unclaimed {p} has a shard");
        }
    }
}
