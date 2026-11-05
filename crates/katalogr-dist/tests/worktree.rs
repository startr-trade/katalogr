//! FEAT-085 T2/T3: a linked worktree answers from its own checkout. Needs the `.tx` test
//! extractor: `cargo test -p katalogr-dist --features test-extractor`.
//!
//! Tests marked `#[ignore]` assert per-checkout context isolation, which comes from FEAT-043
//! (contexts) and is not merged yet; they are the contract to run once it is.
#![cfg(feature = "test-extractor")]

mod worktree {
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
        main: PathBuf,
        wt: PathBuf,
        kx: PathBuf,
    }

    /// A main checkout with `a.tx` committed and the marker untracked, plus a linked worktree.
    fn world() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let main = root.join("app");
        let wt = root.join("app-wt");
        let kx = root.join("kx");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        std::fs::write(main.join("a.tx"), "sym fn basefn\n").unwrap();
        std::fs::write(main.join(".gitignore"), ".katalogr\n").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-q", "-m", "base"]);
        git(
            &main,
            &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
        );
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), main.clone())], true).unwrap();
        assert!(!wt.join(".katalogr").exists(), "marker must be untracked");
        World {
            _tmp: tmp,
            main,
            wt,
            kx,
        }
    }

    fn open(w: &World, dir: &Path) -> Katalogr {
        Katalogr::resolve(
            dir,
            &Overrides {
                kindex: None,
                project: None,
            },
        )
        .unwrap_or_else(|e| panic!("resolve {}: {e} (kx {})", dir.display(), w.kx.display()))
    }

    fn names(k: &Katalogr, terms: &str) -> Vec<String> {
        match k.search(terms, 20, &Options::default()).unwrap().outcome {
            Outcome::Found(hits) => hits.iter().map(|h| format!("{h:?}")).collect(),
            _ => Vec::new(),
        }
    }

    fn has(k: &Katalogr, terms: &str) -> bool {
        !names(k, terms).is_empty()
    }

    #[test]
    fn linked_answers_from_own_code() {
        let w = world();
        std::fs::write(w.wt.join("b.tx"), "sym fn wtonlyfn\n").unwrap();
        let k = open(&w, &w.wt);
        k.index(None, false, None, &Options::default()).unwrap();
        assert!(has(&k, "wtonlyfn"));
        assert!(has(&k, "basefn"));
        // The main checkout's file on disk is untouched by the worktree's edit.
        assert!(!w.main.join("b.tx").exists());
    }

    #[test]
    #[ignore = "blocked: needs FEAT-043 contexts (per-checkout overlay)"]
    fn two_worktrees_do_not_see_each_other() {
        let w = world();
        std::fs::write(w.wt.join("b.tx"), "sym fn wtonlyfn\n").unwrap();
        std::fs::write(w.main.join("c.tx"), "sym fn mainonlyfn\n").unwrap();
        let kw = open(&w, &w.wt);
        let km = open(&w, &w.main);
        kw.index(None, false, None, &Options::default()).unwrap();
        km.index(None, false, None, &Options::default()).unwrap();
        assert!(has(&kw, "wtonlyfn") && !has(&kw, "mainonlyfn"));
        assert!(has(&km, "mainonlyfn") && !has(&km, "wtonlyfn"));
    }

    #[test]
    #[ignore = "blocked: needs FEAT-043 contexts (per-checkout overlay)"]
    fn main_checkout_unaffected_by_linked_edit() {
        let w = world();
        let km = open(&w, &w.main);
        km.index(None, false, None, &Options::default()).unwrap();
        std::fs::write(w.wt.join("a.tx"), "sym fn editedfn\n").unwrap();
        let kw = open(&w, &w.wt);
        kw.index(None, false, None, &Options::default()).unwrap();
        assert!(has(&km, "basefn") && !has(&km, "editedfn"));
        assert!(has(&kw, "editedfn") && !has(&kw, "basefn"));
    }

    #[test]
    #[ignore = "blocked: needs FEAT-043 contexts (per-checkout overlay)"]
    fn copied_marker_does_not_write_shared_shards() {
        let w = world();
        let km = open(&w, &w.main);
        km.index(None, false, None, &Options::default()).unwrap();
        let shards = || {
            let mut v: Vec<_> = walk(&w.kx)
                .into_iter()
                .map(|p| (p.clone(), std::fs::read(&p).unwrap()))
                .collect();
            v.sort();
            v
        };
        let before = shards();
        std::fs::copy(w.main.join(".katalogr"), w.wt.join(".katalogr")).unwrap();
        std::fs::write(w.wt.join("b.tx"), "sym fn wtonlyfn\n").unwrap();
        let kw = open(&w, &w.wt);
        kw.index(None, false, None, &Options::default()).unwrap();
        assert_eq!(before, shards());
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }

    #[test]
    #[ignore = "blocked: needs FEAT-043 contexts (per-checkout overlay)"]
    fn notify_routes_to_own_context() {
        let w = world();
        let km = open(&w, &w.main);
        km.index(None, false, None, &Options::default()).unwrap();
        std::fs::write(w.wt.join("b.tx"), "sym fn wtonlyfn\n").unwrap();
        let kw = open(&w, &w.wt);
        kw.notify(&["b.tx".to_string()], false).unwrap();
        kw.status(&Options::default()).unwrap();
        assert!(has(&kw, "wtonlyfn"));
        assert!(!has(&km, "wtonlyfn"));
    }

    #[test]
    #[ignore = "blocked: needs FEAT-043 contexts (per-checkout overlay); the git-hook path runs `notify --from-git` with the worktree as cwd"]
    fn git_hook_routes_to_own_context() {
        let w = world();
        let km = open(&w, &w.main);
        km.index(None, false, None, &Options::default()).unwrap();
        std::fs::write(w.wt.join("b.tx"), "sym fn wtonlyfn\n").unwrap();
        git(&w.wt, &["add", "b.tx"]);
        git(&w.wt, &["commit", "-q", "-m", "wt"]);
        let kw = open(&w, &w.wt);
        kw.index(
            Some("HEAD~1..HEAD".into()),
            false,
            None,
            &Options::default(),
        )
        .unwrap();
        assert!(has(&kw, "wtonlyfn"));
        assert!(!has(&km, "wtonlyfn"));
    }
}
