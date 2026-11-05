//! Branch and working-tree contexts (FEAT-043).

mod context {
    use katalogr_dist::contexts::ContextChoice;
    use katalogr_dist::{Katalogr, Options, Overrides};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env_remove("KATALOGR_CONTEXT")
            .args(args)
            .output()
            .expect("run katalogr");
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).expect("utf8"),
            String::from_utf8(out.stderr).expect("utf8"),
        )
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let (code, out, err) = run(dir, args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        out
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    struct P {
        _tmp: tempfile::TempDir,
        kx: PathBuf,
        repo: PathBuf,
    }

    /// A project `demo` with a marked repo `app` holding `files`; a git repository on `main`
    /// with the files committed unless `use_git` is false.
    fn project(files: &[(&str, &str)], use_git: bool) -> P {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        if use_git {
            git(&repo, &["init", "-q", "-b", "main"]);
            git(&repo, &["config", "user.name", "Test"]);
            git(&repo, &["config", "user.email", "test@example.com"]);
            git(&repo, &["config", "commit.gpgsign", "false"]);
        }
        ok(tmp.path(), &["kindex", "init", kx.to_str().unwrap()]);
        let spec = format!("app={}", repo.display());
        ok(
            tmp.path(),
            &[
                "--kindex",
                kx.to_str().unwrap(),
                "project",
                "init",
                "demo",
                "--repo",
                &spec,
                "--mark",
            ],
        );
        for (n, t) in files {
            std::fs::write(repo.join(n), t).unwrap();
        }
        if use_git {
            git(&repo, &["add", "-A"]);
            git(&repo, &["commit", "-q", "-m", "c1"]);
        }
        P {
            _tmp: tmp,
            kx,
            repo,
        }
    }

    fn katalogr(at: &Path, c: ContextChoice) -> Katalogr {
        let mut k = Katalogr::resolve(at, &Overrides::default()).unwrap();
        k.set_context(c);
        k
    }

    /// The answer to `find text`, as text, in a context.
    fn find(at: &Path, c: ContextChoice, text: &str) -> String {
        let r = katalogr(at, c).find(text, &Options::default()).unwrap();
        format!("{:?}", r.outcome)
    }

    /// Whether `find text` found a symbol.
    fn has(at: &Path, c: ContextChoice, text: &str) -> bool {
        find(at, c, text).starts_with("Found")
    }

    fn layer_root(p: &P) -> PathBuf {
        p.kx.join("projects/demo/.cache/contexts")
    }

    fn layers(p: &P) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(layer_root(p))
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    fn shard_count(dir: &Path) -> usize {
        fn walk(d: &Path, n: &mut usize) {
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, n);
                    } else if p.to_string_lossy().ends_with(".kshard.json") {
                        *n += 1;
                    }
                }
            }
        }
        let mut n = 0;
        walk(&dir.join("shards"), &mut n);
        n
    }

    const FILES: [(&str, &str); 3] = [
        ("a.tx", "sym fn Alpha\n"),
        ("b.tx", "sym fn Beta\nlink Beta calls Alpha\n"),
        ("c.tx", "sym fn Gamma\n"),
    ];

    /// A project whose base already holds the committed files.
    fn based() -> P {
        let p = project(&FILES, true);
        katalogr(&p.repo, ContextChoice::Base)
            .advance_base(&Options::default())
            .unwrap();
        p
    }

    #[test]
    fn first_read_seeds_then_answers() {
        let p = based();
        assert!(layers(&p).is_empty());
        let got = find(&p.repo, ContextChoice::Current, "Alpha");
        assert!(got.contains("Alpha"), "{got}");
        let ls = layers(&p);
        assert_eq!(ls.len(), 1);
        assert!(ls[0].join("context.json").is_file());
        // A second read does not seed again and answers the same.
        assert_eq!(find(&p.repo, ContextChoice::Current, "Alpha"), got);
        assert_eq!(layers(&p).len(), 1);
    }

    #[test]
    fn overlay_holds_only_delta() {
        let p = based();
        find(&p.repo, ContextChoice::Current, "Alpha");
        let layer = layers(&p).remove(0);
        // Nothing differs from the base yet.
        assert_eq!(shard_count(&layer), 0);
        std::fs::write(p.repo.join("c.tx"), "sym fn Gamma\nsym fn Delta\n").unwrap();
        ok(&p.repo, &["notify", "c.tx"]);
        assert!(has(&p.repo, ContextChoice::Current, "Delta"));
        assert_eq!(shard_count(&layer), 1);
        // The base is untouched by the edit.
        assert!(!has(&p.repo, ContextChoice::Base, "Delta"));
    }

    #[test]
    fn contexts_are_isolated() {
        let p = based();
        let wt = p.repo.with_file_name("wt");
        git(
            &p.repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "side"],
        );
        std::fs::copy(p.repo.join(".katalogr"), wt.join(".katalogr")).unwrap();
        std::fs::write(p.repo.join("a.tx"), "sym fn AlphaMain\n").unwrap();
        std::fs::write(wt.join("a.tx"), "sym fn AlphaSide\n").unwrap();
        ok(&p.repo, &["notify", "a.tx"]);
        ok(&wt, &["notify", "a.tx"]);
        assert!(has(&p.repo, ContextChoice::Current, "AlphaMain"));
        assert!(!has(&p.repo, ContextChoice::Current, "AlphaSide"));
        assert!(has(&wt, ContextChoice::Current, "AlphaSide"));
        assert!(!has(&wt, ContextChoice::Current, "AlphaMain"));
        assert_eq!(layers(&p).len(), 2);
    }

    #[test]
    fn matches_clean_full_index() {
        let p = based();
        find(&p.repo, ContextChoice::Current, "Alpha");
        std::fs::write(p.repo.join("a.tx"), "sym fn Alpha\nsym fn Alpha2\n").unwrap();
        std::fs::remove_file(p.repo.join("c.tx")).unwrap();
        std::fs::write(
            p.repo.join("d.tx"),
            "sym fn Delta\nlink Delta calls Alpha2\n",
        )
        .unwrap();
        ok(&p.repo, &["notify", "a.tx", "d.tx"]);
        ok(&p.repo, &["notify", "--deleted", "c.tx"]);

        // The same files, indexed from scratch in a project of their own.
        let q = project(&[], false);
        for f in ["a.tx", "b.tx", "d.tx"] {
            std::fs::copy(p.repo.join(f), q.repo.join(f)).unwrap();
        }
        ok(&q.repo, &["index"]);
        for name in ["Alpha", "Alpha2", "Beta", "Gamma", "Delta"] {
            let a = find(&p.repo, ContextChoice::Current, name);
            let b = find(&q.repo, ContextChoice::Current, name);
            assert_eq!(a, b, "{name}");
        }
        for name in ["Alpha", "Alpha2"] {
            let a = katalogr(&p.repo, ContextChoice::Current)
                .callers(name, 2, &Options::default())
                .unwrap();
            let b = katalogr(&q.repo, ContextChoice::Current)
                .callers(name, 2, &Options::default())
                .unwrap();
            assert_eq!(
                format!("{:?}", a.outcome),
                format!("{:?}", b.outcome),
                "{name}"
            );
        }
    }

    #[test]
    fn plain_directory_keeps_one_layer() {
        let p = project(&FILES, false);
        ok(&p.repo, &["index"]);
        assert!(has(&p.repo, ContextChoice::Current, "Alpha"));
        assert!(layers(&p).is_empty());
        assert!(p.kx.join("projects/demo/.cache/index.redb").is_file());
    }

    #[test]
    fn kindex_clean_after_uncommitted_edit() {
        let p = based();
        git(&p.kx, &["init", "-q"]);
        git(&p.kx, &["config", "user.name", "Test"]);
        git(&p.kx, &["config", "user.email", "test@example.com"]);
        git(&p.kx, &["config", "commit.gpgsign", "false"]);
        git(&p.kx, &["add", "-A"]);
        git(&p.kx, &["commit", "-q", "-m", "base"]);
        find(&p.repo, ContextChoice::Current, "Alpha");
        std::fs::write(p.repo.join("c.tx"), "sym fn Gamma\nsym fn Uncommitted\n").unwrap();
        ok(&p.repo, &["notify", "c.tx"]);
        assert!(has(&p.repo, ContextChoice::Current, "Uncommitted"));
        assert_eq!(git(&p.kx, &["status", "--porcelain"]), "");
    }

    #[test]
    fn branch_switch_touches_only_diff() {
        let p = based();
        find(&p.repo, ContextChoice::Current, "Alpha");
        git(&p.repo, &["checkout", "-q", "-b", "side"]);
        std::fs::write(p.repo.join("c.tx"), "sym fn GammaSide\n").unwrap();
        git(&p.repo, &["commit", "-qam", "side"]);
        let side = git(&p.repo, &["rev-parse", "HEAD"]);
        git(&p.repo, &["checkout", "-q", "main"]);
        let main = git(&p.repo, &["rev-parse", "HEAD"]);
        // The checkout moves from `side` to `main`: only the file that differs is queued.
        ok(
            &p.repo,
            &["notify", "--from-git", "post-checkout", &side, &main, "1"],
        );
        let layer = layers(&p).remove(0);
        let mut queued = Vec::new();
        for f in std::fs::read_dir(layer.join(".cache/inbox"))
            .unwrap()
            .flatten()
        {
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(f.path()).unwrap()).unwrap();
            queued.push((
                v["path"].as_str().unwrap().to_string(),
                v["kind"].to_string(),
            ));
        }
        assert_eq!(
            queued,
            vec![("c.tx".to_string(), "\"changed\"".to_string())]
        );
        // The base was not asked to do anything for a branch that is not the default.
        assert!(has(&p.repo, ContextChoice::Current, "Gamma"));
        assert!(!has(&p.repo, ContextChoice::Current, "GammaSide"));
    }
}
