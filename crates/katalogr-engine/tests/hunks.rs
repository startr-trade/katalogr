mod scan {
    use katalogr_engine::scan::{ChangeKind, FileChange, ScanError, changed_hunks};
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

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("a.tx"), "l1\nl2\nl3\nl4\nl5\n").unwrap();
        std::fs::write(root.join("gone.tx"), "x\n").unwrap();
        std::fs::write(root.join("same.tx"), "s\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "c1"]);
        dir
    }

    fn find<'a>(v: &'a [FileChange], p: &str) -> &'a FileChange {
        v.iter().find(|c| c.path.as_str() == p).unwrap()
    }

    #[test]
    fn hunks_for_committed_range() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join("a.tx"), "l1\nL2\nl3\nl4\nL5\n").unwrap();
        git(root, &["commit", "-q", "-am", "c2"]);
        let v = changed_hunks(root, "HEAD~1", Some("HEAD")).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, ChangeKind::Modified);
        assert_eq!(v[0].ranges, vec![(2, 2), (5, 5)]);
        assert!(
            changed_hunks(root, "HEAD", Some("HEAD"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn hunks_include_uncommitted() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join("a.tx"), "l1\nl2\nNEW\nl3\nl4\nl5\n").unwrap();
        let v = changed_hunks(root, "HEAD", None).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].path.as_str(), "a.tx");
        assert_eq!(v[0].ranges, vec![(3, 3)]);
        // committed range does not see it
        assert!(
            changed_hunks(root, "HEAD", Some("HEAD"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn added_and_deleted_have_no_ranges() {
        let dir = repo();
        let root = dir.path();
        std::fs::remove_file(root.join("gone.tx")).unwrap();
        std::fs::write(root.join("new.tx"), "n\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "c2"]);
        let v = changed_hunks(root, "HEAD~1", Some("HEAD")).unwrap();
        let g = find(&v, "gone.tx");
        assert_eq!((g.kind, g.ranges.len()), (ChangeKind::Deleted, 0));
        let n = find(&v, "new.tx");
        assert_eq!((n.kind, n.ranges.len()), (ChangeKind::Added, 0));
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn bad_revision_errors() {
        let dir = repo();
        match changed_hunks(dir.path(), "no-such-rev", None) {
            Err(ScanError::BadRevision { rev, .. }) => assert_eq!(rev, "no-such-rev"),
            other => panic!("expected BadRevision, got {other:?}"),
        }
    }
}

mod hunks_status {
    use katalogr_engine::scan::{ChangeKind, changed_hunks};
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .current_dir(root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn staged_deleted_and_untracked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("a.tx"), "1\n2\n").unwrap();
        std::fs::write(root.join("b.tx"), "x\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "c1"]);
        std::fs::write(root.join("a.tx"), "1\nTWO\n").unwrap();
        git(root, &["add", "a.tx"]);
        std::fs::remove_file(root.join("b.tx")).unwrap();
        std::fs::write(root.join("untracked.tx"), "u\n").unwrap();
        let v = changed_hunks(root, "HEAD", None).unwrap();
        let got: Vec<_> = v.iter().map(|c| (c.path.as_str(), c.kind)).collect();
        assert_eq!(
            got,
            [
                ("a.tx", ChangeKind::Modified),
                ("b.tx", ChangeKind::Deleted)
            ]
        );
    }
}
