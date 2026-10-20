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
}
