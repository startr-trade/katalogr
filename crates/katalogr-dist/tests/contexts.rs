//! FEAT-118: a checkout context of a two-repo project catches its other repository up to the
//! commit it reports, read from git. Needs the `.tx` test extractor:
//! `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod index {
    use katalogr_dist::{Katalogr, Options, Outcome, Overrides, kindex_init, project_init};
    use std::path::{Path, PathBuf};

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    struct World {
        _tmp: tempfile::TempDir,
        app: PathBuf,
        other: PathBuf,
    }

    /// Two committed repos, `app` (marked) and `other`, in one project.
    fn world() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (app, other, kx) = (root.join("app"), root.join("other"), root.join("kx"));
        for (dir, text) in [(&app, "sym fn appfn\n"), (&other, "sym fn otherfn\n")] {
            std::fs::create_dir_all(dir).unwrap();
            git(dir, &["init", "-q", "-b", "main"]);
            std::fs::write(dir.join("a.tx"), text).unwrap();
            std::fs::write(dir.join(".gitignore"), ".katalogr\n").unwrap();
            git(dir, &["add", "."]);
            git(dir, &["commit", "-q", "-m", "c1"]);
        }
        kindex_init(&kx).unwrap();
        project_init(
            &kx,
            "demo",
            vec![("app".into(), app.clone()), ("other".into(), other.clone())],
            true,
        )
        .unwrap();
        let w = World {
            _tmp: tmp,
            app,
            other,
        };
        open(&w)
            .index(None, false, None, &Options::default())
            .unwrap();
        w
    }

    /// The checkout context of `app`.
    fn open(w: &World) -> Katalogr {
        Katalogr::resolve(&w.app, &Overrides::default()).unwrap()
    }

    fn has(k: &Katalogr, terms: &str) -> bool {
        match k.search(terms, 20, &Options::default()).unwrap().outcome {
            Outcome::Found(hits) => !hits.is_empty(),
            _ => false,
        }
    }

    fn commit_edit(w: &World, file: &str, text: &str) {
        std::fs::write(w.other.join(file), text).unwrap();
        git(&w.other, &["add", "."]);
        git(&w.other, &["commit", "-q", "-m", "edit"]);
    }

    /// FEAT-118 R-1.
    #[test]
    fn second_repo_edit_seen() {
        let w = world();
        assert!(has(&open(&w), "otherfn"));
        commit_edit(&w, "b.tx", "sym fn newotherfn\n");
        let k = open(&w);
        assert!(!has(&k, "newotherfn"));
        k.index(None, false, None, &Options::default()).unwrap();
        assert!(has(&k, "newotherfn"));
        let r = k
            .search("newotherfn", 5, &Options::default())
            .unwrap()
            .report;
        assert!(r.exact, "{r:?}");
    }

    /// FEAT-118 R-2.
    #[test]
    fn second_repo_uncommitted_edit_not_seen() {
        let w = world();
        std::fs::write(w.other.join("b.tx"), "sym fn dirtyotherfn\n").unwrap();
        let k = open(&w);
        k.index(None, false, None, &Options::default()).unwrap();
        assert!(!has(&k, "dirtyotherfn"));
        assert!(has(&k, "otherfn"));
    }

    /// FEAT-118 R-2.
    #[test]
    fn full_does_not_leak_uncommitted_other_repo() {
        let w = world();
        std::fs::write(w.other.join("b.tx"), "sym fn dirtyotherfn\n").unwrap();
        let k = open(&w);
        k.index(None, true, None, &Options::default()).unwrap();
        assert!(!has(&k, "dirtyotherfn"));
        assert!(has(&k, "otherfn"));
        // Committed, it is seen even though the file on disk did not change.
        git(&w.other, &["add", "."]);
        git(&w.other, &["commit", "-q", "-m", "edit"]);
        k.index(None, true, None, &Options::default()).unwrap();
        assert!(has(&k, "dirtyotherfn"));
    }
}
