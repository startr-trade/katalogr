//! FEAT-085 T1: marker discovery through the main checkout of a linked worktree. Unit tests:
//! only files are written, no process and no git.

mod marker {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_store::StoreError;
    use katalogr_store::kindex::ProjectName;
    use katalogr_store::marker::{self, Marker, Overrides};

    /// The canonical form without Windows' verbatim prefix, as the product resolves roots:
    /// joining `../kx` onto a verbatim path collapses the `..`, so `main/../kx` and
    /// `wt/../kx` would compare equal.
    fn utf8(p: std::path::PathBuf) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(katalogr_store::canon::canonicalize(&p).unwrap()).unwrap()
    }

    /// `<tmp>/main` with `.git/` and `.git/worktrees/wt/commondir`; `<tmp>/wt` with a `.git`
    /// file pointing at it. Returns (main, worktree).
    fn layout(tmp: &Utf8Path, common_rel: &str) -> (Utf8PathBuf, Utf8PathBuf) {
        let main = tmp.join("main");
        let wt = tmp.join("wt");
        let admin = main.join(".git/worktrees/wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(admin.join("commondir"), format!("{common_rel}\n")).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {admin}\n")).unwrap();
        (main, wt)
    }

    fn mark(main: &Utf8Path) {
        let m = Marker {
            kindex: Utf8PathBuf::from("../kx"),
            project: ProjectName::new("demo").unwrap(),
            repo: "alpha".into(),
        };
        marker::write(main, &m).unwrap();
    }

    fn tmp() -> (tempfile::TempDir, Utf8PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let p = utf8(t.path().to_path_buf());
        (t, p)
    }

    #[test]
    fn linked_worktree_finds_main_marker() {
        let (_t, p) = tmp();
        let (main, wt) = layout(&p, "../..");
        mark(&main);
        let nested = wt.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();

        let (w, m) = marker::main_checkout_of(&nested).unwrap();
        assert_eq!(w, utf8(wt.clone().into()));
        assert_eq!(m, utf8(main.clone().into()));

        let r = marker::resolve_with_env(&nested, &Overrides::default(), &|_| None).unwrap();
        assert!(r.via_main_checkout);
        assert_eq!(r.repo.as_deref(), Some("alpha"));
        assert_eq!(r.repo_root.as_deref(), Some(w.as_path()));

        // A marker in the worktree itself wins, and is not "via main".
        mark(&wt);
        let r = marker::resolve_with_env(&nested, &Overrides::default(), &|_| None).unwrap();
        assert!(!r.via_main_checkout);
        assert_eq!(r.repo_root.as_deref(), Some(wt.as_path()));
    }

    #[test]
    fn relative_kindex_resolves_from_main() {
        let (_t, p) = tmp();
        let (main, wt) = layout(&p, "../..");
        mark(&main);
        let r = marker::resolve_with_env(&wt, &Overrides::default(), &|_| None).unwrap();
        let main = utf8(main.into());
        assert_eq!(r.kindex, main.join("../kx"));
        assert_ne!(r.kindex, utf8(wt.into()).join("../kx"));
    }

    #[test]
    fn malformed_git_file_is_not_an_error() {
        let (_t, p) = tmp();
        let wt = p.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        for body in [
            "",
            "garbage",
            "gitdir:",
            "gitdir: /nonexistent/place/.git/worktrees/x",
        ] {
            std::fs::write(wt.join(".git"), body).unwrap();
            assert_eq!(marker::main_checkout_of(&wt), None, "{body:?}");
            assert!(matches!(
                marker::resolve_with_env(&wt, &Overrides::default(), &|_| None),
                Err(StoreError::NotConfigured)
            ));
        }
        // A gitdir whose commondir file is missing or empty.
        let admin = p.join("admin");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {admin}\n")).unwrap();
        assert_eq!(marker::main_checkout_of(&wt), None);
        std::fs::write(admin.join("commondir"), "").unwrap();
        assert_eq!(marker::main_checkout_of(&wt), None);
        // A plain checkout (`.git` directory) is not a linked worktree.
        let plain = p.join("plain");
        std::fs::create_dir_all(plain.join(".git")).unwrap();
        assert_eq!(marker::main_checkout_of(&plain), None);
    }

    #[test]
    fn bare_common_dir_not_configured() {
        let (_t, p) = tmp();
        // The common directory is `bare.git`, which does not end in `.git` as a directory name
        // of a checkout's metadata: there is no main checkout.
        let bare = p.join("bare.git");
        let admin = bare.join("worktrees/wt");
        let wt = p.join("wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {admin}\n")).unwrap();
        assert_eq!(marker::main_checkout_of(&wt), None);
        assert!(matches!(
            marker::resolve_with_env(&wt, &Overrides::default(), &|_| None),
            Err(StoreError::NotConfigured)
        ));
        // Flags still work.
        let o = Overrides {
            kindex: Some("/flag/kx".into()),
            project: Some(ProjectName::new("flagproj").unwrap()),
        };
        let r = marker::resolve_with_env(&wt, &o, &|_| None).unwrap();
        assert_eq!(r.repo, None);
        assert!(!r.via_main_checkout);
    }
}
