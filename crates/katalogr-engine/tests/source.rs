mod source {
    use camino::Utf8PathBuf;
    use katalogr_engine::FileSource;
    use katalogr_model::RelPath;
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn repo() -> (tempfile::TempDir, Utf8PathBuf, String) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::create_dir_all(root.join("vendor")).unwrap();
        std::fs::write(root.join("a.tx"), "sym fn A\n").unwrap();
        std::fs::write(root.join("src/b.tx"), "sym fn B\n").unwrap();
        std::fs::write(root.join("src/deep/c.tx"), "sym fn C\n").unwrap();
        std::fs::write(root.join("vendor/v.tx"), "sym fn V\n").unwrap();
        std::fs::write(root.join("notes.md"), "n\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "c1"]);
        let head = git(root, &["rev-parse", "HEAD"]);
        let p = Utf8PathBuf::from_path_buf(root.to_path_buf()).unwrap();
        (t, p, head)
    }

    fn rel(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }

    fn commit(root: &Utf8PathBuf, head: &str) -> FileSource {
        FileSource::Commit {
            repo_root: root.clone(),
            commit: head.to_string(),
        }
    }

    #[test]
    fn commit_reads_committed_bytes() {
        let (_t, root, head) = repo();
        let s = commit(&root, &head);
        assert_eq!(s.read(&rel("src/b.tx")).unwrap().unwrap(), b"sym fn B\n");
        assert_eq!(s.read(&rel("nope.tx")).unwrap(), None);
        // A directory is not a file.
        assert_eq!(s.read(&rel("src")).unwrap(), None);
        // Another commit reads its own bytes.
        std::fs::write(root.join("a.tx"), "sym fn A2\n").unwrap();
        git(root.as_std_path(), &["commit", "-qam", "c2"]);
        let head2 = git(root.as_std_path(), &["rev-parse", "HEAD"]);
        assert_eq!(s.read(&rel("a.tx")).unwrap().unwrap(), b"sym fn A\n");
        assert_eq!(
            commit(&root, &head2).read(&rel("a.tx")).unwrap().unwrap(),
            b"sym fn A2\n"
        );
    }

    #[test]
    fn commit_ignores_dirty_disk() {
        let (_t, root, head) = repo();
        std::fs::write(root.join("a.tx"), "sym fn Dirty\n").unwrap();
        std::fs::remove_file(root.join("src/b.tx")).unwrap();
        std::fs::write(root.join("new.tx"), "sym fn N\n").unwrap();
        let s = commit(&root, &head);
        assert_eq!(s.read(&rel("a.tx")).unwrap().unwrap(), b"sym fn A\n");
        assert!(s.read(&rel("src/b.tx")).unwrap().is_some());
        assert_eq!(s.read(&rel("new.tx")).unwrap(), None);
        let all = ["**".to_string()];
        let listed = s.list(&all, &[]).unwrap();
        assert!(listed.contains(&rel("src/b.tx")));
        assert!(!listed.contains(&rel("new.tx")));
        // The disk source sees the dirty state.
        let d = FileSource::Disk(root.clone());
        assert_eq!(d.read(&rel("a.tx")).unwrap().unwrap(), b"sym fn Dirty\n");
        assert_eq!(d.read(&rel("src/b.tx")).unwrap(), None);
    }

    #[test]
    fn commit_listing_matches_disk_when_clean() {
        let (_t, root, head) = repo();
        let c = commit(&root, &head);
        let d = FileSource::Disk(root.clone());
        let cases: [(&[&str], &[&str]); 3] = [
            (&["**"], &[]),
            (&["**/*.tx"], &["vendor/**"]),
            (&["src/**"], &["**/c.tx"]),
        ];
        for (inc, exc) in cases {
            let inc: Vec<String> = inc.iter().map(|s| s.to_string()).collect();
            let exc: Vec<String> = exc.iter().map(|s| s.to_string()).collect();
            let a = c.list(&inc, &exc).unwrap();
            assert_eq!(a, d.list(&inc, &exc).unwrap(), "{inc:?} {exc:?}");
            assert!(!a.is_empty());
        }
    }
}
