mod base {
    use katalogr_engine::base::{branch_tip, default_branch, plan_base_advance};
    use katalogr_engine::{EventKind, NewEvent};
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

    fn repo(branch: &str) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        git(r, &["init", "-q", "-b", branch]);
        git(r, &["config", "user.name", "Test"]);
        git(r, &["config", "user.email", "test@example.com"]);
        git(r, &["config", "commit.gpgsign", "false"]);
        std::fs::write(r.join("a.tx"), "sym fn A\n").unwrap();
        std::fs::write(r.join("b.tx"), "sym fn B\n").unwrap();
        git(r, &["add", "."]);
        git(r, &["commit", "-q", "-m", "c1"]);
        t
    }

    #[test]
    fn default_branch_order() {
        // Neither main nor master: none.
        let t = repo("trunk");
        let r = t.path();
        assert_eq!(default_branch(r, None).unwrap(), None);
        // The configured name wins over everything.
        assert_eq!(
            default_branch(r, Some("dev")).unwrap().as_deref(),
            Some("dev")
        );
        // master, then main before master.
        git(r, &["branch", "master"]);
        assert_eq!(default_branch(r, None).unwrap().as_deref(), Some("master"));
        git(r, &["branch", "main"]);
        assert_eq!(default_branch(r, None).unwrap().as_deref(), Some("main"));
        // The remote's HEAD comes first.
        let head = git(r, &["rev-parse", "HEAD"]);
        git(r, &["update-ref", "refs/remotes/origin/trunk", &head]);
        git(
            r,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        assert_eq!(default_branch(r, None).unwrap().as_deref(), Some("trunk"));
        assert_eq!(
            default_branch(r, Some("main")).unwrap().as_deref(),
            Some("main")
        );
        // A tip falls back to the remote-tracking ref.
        assert_eq!(
            branch_tip(r, "trunk").unwrap().as_deref(),
            Some(head.as_str())
        );
        assert_eq!(branch_tip(r, "nope").unwrap(), None);
    }

    #[test]
    fn advance_queues_only_diff() {
        let t = repo("main");
        let r = t.path();
        let c1 = git(r, &["rev-parse", "HEAD"]);
        std::fs::write(r.join("a.tx"), "sym fn A2\n").unwrap();
        std::fs::remove_file(r.join("b.tx")).unwrap();
        std::fs::write(r.join("c.tx"), "sym fn C\n").unwrap();
        git(r, &["add", "-A"]);
        git(r, &["commit", "-q", "-m", "c2"]);
        let c2 = git(r, &["rev-parse", "HEAD"]);
        // Dirty disk and untracked files do not matter.
        std::fs::write(r.join("untracked.tx"), "sym fn U\n").unwrap();

        let rel = |s: &str| RelPath::new(s).unwrap();
        let evs = plan_base_advance("app", r, Some(&c1), &c2).unwrap();
        assert_eq!(
            evs,
            vec![
                NewEvent::file("app", &rel("a.tx"), EventKind::Changed),
                NewEvent::file("app", &rel("b.tx"), EventKind::Deleted),
                NewEvent::file("app", &rel("c.tx"), EventKind::Changed),
            ]
        );
        assert!(
            plan_base_advance("app", r, Some(&c2), &c2)
                .unwrap()
                .is_empty()
        );
        // No recorded commit, or one that no longer resolves: a rescan.
        assert_eq!(
            plan_base_advance("app", r, None, &c2).unwrap(),
            vec![NewEvent::rescan("app")]
        );
        let gone = "0123456789012345678901234567890123456789";
        assert_eq!(
            plan_base_advance("app", r, Some(gone), &c2).unwrap(),
            vec![NewEvent::rescan("app")]
        );
    }
}
