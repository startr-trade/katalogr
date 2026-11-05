//! FEAT-085 T2/T3: a linked worktree answers from its own checkout. Needs the `.tx` test
//! extractor: `cargo test -p katalogr-dist --features test-extractor`.
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

    fn no_overrides() -> Overrides {
        Overrides::default()
    }

    fn open_in(dir: &Path, context: Option<String>) -> Katalogr {
        Katalogr::resolve(
            dir,
            &Overrides {
                context,
                ..Overrides::default()
            },
        )
        .unwrap()
    }

    fn open(w: &World, dir: &Path) -> Katalogr {
        Katalogr::resolve(dir, &no_overrides())
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
    fn copied_marker_does_not_write_shared_shards() {
        let w = world();
        let km = open(&w, &w.main);
        km.index(None, false, None, &Options::default()).unwrap();
        let shards = || {
            let mut v: Vec<_> = walk(&w.kx)
                .into_iter()
                .filter(|p| {
                    !p.components().any(|c| {
                        let c = c.as_os_str();
                        c == "contexts" || c == ".git" || c == ".cache"
                    })
                })
                .map(|p| (p.clone(), std::fs::read(&p).unwrap()))
                .collect();
            v.sort();
            v
        };
        let base = || open_in(&w.main, Some("base".into()));
        let before = shards();
        let mark = base().status(&Options::default()).unwrap().watermark;
        std::fs::copy(w.main.join(".katalogr"), w.wt.join(".katalogr")).unwrap();
        std::fs::write(w.wt.join("b.tx"), "sym fn wtonlyfn\n").unwrap();
        let kw = open(&w, &w.wt);
        kw.index(None, false, None, &Options::default()).unwrap();
        // The base index (a local cache, rewritten on open) holds the same content: nothing
        // of the worktree reached it.
        assert_eq!(
            base().status(&Options::default()).unwrap().watermark,
            mark,
            "the base advanced"
        );
        assert!(!has(&base(), "wtonlyfn"));
        let after = shards();
        let changed: Vec<_> = before
            .iter()
            .map(|(p, _)| p)
            .chain(after.iter().map(|(p, _)| p))
            .filter(|p| before.iter().find(|(q, _)| q == *p) != after.iter().find(|(q, _)| q == *p))
            .collect();
        assert!(changed.is_empty(), "shared files changed: {changed:?}");
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
    fn git_hook_routes_to_own_context() {
        let w = world();
        let km = open(&w, &w.main);
        km.index(None, false, None, &Options::default()).unwrap();
        std::fs::write(w.wt.join("b.tx"), "sym fn wtonlyfn\n").unwrap();
        git(&w.wt, &["add", "b.tx"]);
        git(&w.wt, &["commit", "-q", "-m", "wt"]);
        let kw = open(&w, &w.wt);
        let n = katalogr_dist::hooks::notify_from_git(&w.wt, &no_overrides(), "post-commit", &[]);
        assert!(n >= 1, "queued {n}");
        kw.status(&Options::default()).unwrap();
        kw.index(None, false, None, &Options::default()).unwrap();
        assert!(has(&kw, "wtonlyfn"));
        assert!(!has(&km, "wtonlyfn"));
    }

    #[test]
    fn hook_post_search_reads_own_context() {
        use katalogr_dist::hooks::{HookKind, run_hook};
        let w = world();
        std::fs::write(w.wt.join("b.tx"), "sym fn wtonlyfn\n").unwrap();
        let kw = open(&w, &w.wt);
        kw.index(None, false, None, &Options::default()).unwrap();
        let input = |cwd: &Path| {
            serde_json::json!({
                "tool_name": "Grep",
                "tool_input": {"pattern": "wtonlyfn"},
                "cwd": cwd.to_str().unwrap(),
            })
            .to_string()
        };
        let in_wt = run_hook(HookKind::PostSearch, &input(&w.wt), &w.wt, &no_overrides());
        assert!(
            in_wt.is_some_and(|o| o.contains("wtonlyfn")),
            "the worktree's own symbol is briefed"
        );
        let in_main = run_hook(
            HookKind::PostSearch,
            &input(&w.main),
            &w.main,
            &no_overrides(),
        );
        assert!(in_main.is_none(), "the main checkout does not know it");
    }

    #[test]
    fn context_flag_reads_another_checkout() {
        let w = world();
        std::fs::write(w.wt.join("b.tx"), "sym fn wtonlyfn\n").unwrap();
        let kw = open(&w, &w.wt);
        kw.index(None, false, None, &Options::default()).unwrap();
        let id = kw.context_name();
        assert!(id.starts_with("w-"), "{id}");
        let km = open_in(&w.main, Some(id));
        assert!(has(&km, "wtonlyfn"));
        assert!(!has(&open(&w, &w.main), "wtonlyfn"));
        // `base` from inside the worktree does not see the worktree's file.
        assert!(!has(&open_in(&w.wt, Some("base".into())), "wtonlyfn"));
    }
}
