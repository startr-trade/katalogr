mod graph {
    use katalogr_engine::scan::{PathChange, ScanError, changed_paths};
    use katalogr_model::RelPath;
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let st = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?} failed");
    }

    #[test]
    fn reindex_from_diff_range() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "commit.gpgsign", "false"]);

        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        std::fs::write(root.join("b.txt"), "bee\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "c1"]);

        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        std::fs::remove_file(root.join("b.txt")).unwrap();
        std::fs::write(root.join("c.txt"), "sea\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "c2"]);

        let expected = vec![
            PathChange::Modified(RelPath::new("a.txt").unwrap()),
            PathChange::Deleted(RelPath::new("b.txt").unwrap()),
            PathChange::Added(RelPath::new("c.txt").unwrap()),
        ];
        assert_eq!(changed_paths(root, "HEAD~1..HEAD").unwrap(), expected);
        assert_eq!(changed_paths(root, "HEAD~1").unwrap(), expected);

        match changed_paths(root, "no-such-rev") {
            Err(ScanError::BadRevision { rev, .. }) => assert_eq!(rev, "no-such-rev"),
            other => panic!("expected BadRevision, got {other:?}"),
        }
    }
}
