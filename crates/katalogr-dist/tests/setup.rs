//! FEAT-099: `project init --mark` keeps the marker out of the checkout's position.

mod setup {
    use katalogr_dist::{kindex_init, project_init};
    use std::path::Path;

    fn git(root: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?} failed");
    }

    #[test]
    fn mark_excludes_marker_from_position() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "c1"]);
        let kx = tmp.path().join("kx");
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        assert!(repo.join(".katalogr").exists());
        let st = katalogr_engine::position::read_position("app", &repo, &[], &[]).unwrap();
        assert_eq!(st.position.dirty, None);
        // A second init adds no second line.
        project_init(&kx, "demo2", vec![("app".into(), repo.clone())], true).unwrap();
        let text = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
        let n = text
            .lines()
            .filter(|l| l.trim() == format!("/{}", ".katalogr"))
            .count();
        assert_eq!(n, 1, "{text}");
    }
}
