//! FEAT-124: a save in a folder that is not a git checkout re-extracts that file only.

mod support;

mod save {
    use super::support::{full_index, init_project, open_engine};
    use camino::Utf8PathBuf;
    use katalogr_engine::{Consistency, Engine, QueryOptions, Scope, canonical_graph};
    use std::time::Duration;

    fn read(e: &Engine) {
        let o = QueryOptions {
            consistency: Consistency::WaitAll,
            timeout: Duration::from_secs(30),
            scope: Scope::Project,
        };
        e.with_consistency(&o, |tx| canonical_graph(tx)).unwrap();
    }

    /// FEAT-043 R-13 (FEAT-124): in a folder with no `.git`, every file is "uncommitted", yet a
    /// save must queue and re-extract only the file whose bytes changed, not all of them.
    #[test]
    fn save_in_plain_folder_reextracts_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let (kx, work) = (root.join("kx"), root.join("work"));
        std::fs::create_dir_all(&work).unwrap();
        for i in 0..40 {
            std::fs::write(work.join(format!("f{i}.tx")), format!("sym fn F{i}\n")).unwrap();
        }
        init_project(&kx);
        let engine = open_engine(&kx, &work);
        full_index(&engine);
        read(&engine);
        read(&engine);

        let before = engine.index_stats().files;
        std::fs::write(work.join("f7.tx"), "sym fn F7\nsym fn F7b\n").unwrap();
        read(&engine);
        let one = engine.index_stats().files - before;
        assert!(one <= 2, "a save re-extracted {one} files");

        let before = engine.index_stats().files;
        read(&engine);
        assert_eq!(
            engine.index_stats().files - before,
            0,
            "a read re-extracted"
        );
    }
}
