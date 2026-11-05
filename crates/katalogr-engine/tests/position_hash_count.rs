//! FEAT-084 revision: counting hashes needs a process of its own, since the counter is global.

mod position {
    use katalogr_engine::position::{hashed_files, read_position_cached};
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let st = Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?} failed");
    }

    #[test]
    fn head_move_rehashes_only_changed() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        git(r, &["init", "-q"]);
        git(r, &["config", "user.name", "Test"]);
        git(r, &["config", "user.email", "test@example.com"]);
        git(r, &["config", "commit.gpgsign", "false"]);
        for i in 0..30 {
            std::fs::create_dir_all(r.join(format!("d{}", i % 3))).unwrap();
            std::fs::write(
                r.join(format!("d{}/f{i}.txt", i % 3)),
                format!("file {i}\n"),
            )
            .unwrap();
        }
        git(r, &["add", "."]);
        git(r, &["commit", "-q", "-m", "c1"]);
        // Everything is old enough for the cache to trust.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for i in 0..30 {
            let f = std::fs::File::open(r.join(format!("d{}/f{i}.txt", i % 3))).unwrap();
            f.set_modified(old).unwrap();
        }
        for d in ["", "d0", "d1", "d2"] {
            std::fs::File::open(r.join(d))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let cdir = tempfile::tempdir().unwrap();
        let cache = cdir.path().join("r.stat");
        let inc = vec!["**".to_string()];
        let read = || {
            read_position_cached("r", r, &inc, &[], Some(&cache))
                .unwrap()
                .position
        };

        let n0 = hashed_files();
        assert_eq!(read().dirty, None);
        assert_eq!(hashed_files() - n0, 30, "a cold read hashes every file");
        let n1 = hashed_files();
        assert_eq!(read().dirty, None);
        assert_eq!(hashed_files() - n1, 0, "a warm read hashes nothing");

        // One file changes and is committed: HEAD moves, and only that file is read again.
        std::fs::write(r.join("d1/f4.txt"), "edited\n").unwrap();
        git(r, &["commit", "-q", "-a", "-m", "c2"]);
        let n2 = hashed_files();
        let p = read();
        assert_eq!(p.dirty, None);
        assert_eq!(hashed_files() - n2, 1, "only the changed file is hashed");
    }
}
