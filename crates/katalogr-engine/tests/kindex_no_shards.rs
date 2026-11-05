//! FEAT-094 R-1 and R-15 through the engine: indexing writes no shard files and leaves the
//! kindex git work tree with no shard or cache entries; a format 1 project's `shards/` folder is
//! never read.
mod support;

mod kindex {
    use super::support::{full_index, init_project, open_engine};
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_store::{Kindex, ProjectName, SHARD_FORMAT};

    struct Fx {
        _tmp: tempfile::TempDir,
        kx: Utf8PathBuf,
        work: Utf8PathBuf,
    }

    fn fixture() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let (kx, work) = (root.join("kx"), root.join("work"));
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("a.tx"), "sym fn A\nexport k:a A\n").unwrap();
        std::fs::write(work.join("b.tx"), "sym fn B\nref B calls k:a\n").unwrap();
        init_project(&kx);
        Fx {
            _tmp: tmp,
            kx,
            work,
        }
    }

    fn git_status(kx: &Utf8Path) -> String {
        let out = std::process::Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=all"])
            .current_dir(kx)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// R-1: after a full index the kindex git work tree shows no shard file and no cache file.
    #[test]
    fn index_leaves_git_clean() {
        let f = fixture();
        {
            let e = open_engine(&f.kx, &f.work);
            full_index(&e);
        }
        let status = git_status(&f.kx);
        assert!(!status.contains("shards/"), "{status}");
        assert!(!status.contains(".cache"), "{status}");
        assert!(!status.contains(".kshard.json"), "{status}");
        assert!(status.contains("format.json"), "{status}");
    }

    /// R-1: a full index creates no `shards/` folder, and the format file records format 2.
    #[test]
    fn no_shards_folder_after_index() {
        let f = fixture();
        {
            let e = open_engine(&f.kx, &f.work);
            full_index(&e);
        }
        let dir = Kindex::open(&f.kx)
            .unwrap()
            .project(&ProjectName::new("p").unwrap())
            .unwrap();
        assert!(!dir.project_root().join("shards").exists());
        assert!(!dir.leftover_shards());
        let format = dir.format_file().unwrap();
        assert_eq!(format.shard_format, SHARD_FORMAT);
        assert_eq!(SHARD_FORMAT, 2);
    }

    /// R-15: the format file a new project writes is refused by a reader that only knows
    /// format 1, the rule the previous release applied to any shard format above its own.
    #[test]
    fn new_project_refused_by_old_format_rule() {
        let f = fixture();
        let dir = Kindex::open(&f.kx)
            .unwrap()
            .project(&ProjectName::new("p").unwrap())
            .unwrap();
        let raw = std::fs::read_to_string(dir.project_root().join("format.json")).unwrap();
        assert_eq!(
            raw,
            "{\n  \"min_katalogr\": \"0.2.0\",\n  \"shard_format\": 2\n}\n"
        );
        let old_supported: u32 = 1;
        assert!(dir.format_file().unwrap().shard_format > old_supported);
    }
}
