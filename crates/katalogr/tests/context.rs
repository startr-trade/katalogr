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

    /// How many files the layer holds a different revision for than the base: the files the
    /// layer extracted itself (FEAT-043 R-3; the seed carries the base's revisions).
    fn delta_count(p: &P, layer: &Path) -> usize {
        use katalogr_store::{Cache, ContextId, Reader};
        let dl = || std::time::Instant::now() + std::time::Duration::from_secs(5);
        let base = Katalogr::resolve(&p.repo, &Overrides::default())
            .unwrap()
            .project()
            .clone();
        let id = ContextId::new(layer.file_name().unwrap().to_str().unwrap()).unwrap();
        let revisions = |c: Cache| -> std::collections::BTreeMap<_, _> {
            c.read()
                .unwrap()
                .file_revisions()
                .unwrap()
                .into_iter()
                .collect()
        };
        let mine = revisions(Cache::open(&base.layer(&id), dl()).unwrap());
        let theirs = revisions(Cache::open(&base, dl()).unwrap());
        mine.iter()
            .filter(|(k, v)| theirs.get(*k) != Some(*v))
            .count()
            + theirs.keys().filter(|k| !mine.contains_key(*k)).count()
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

    /// FEAT-075 with contexts: a layer is a copy of the base file, so the dictionaries it
    /// carries keep their rows and a seeded layer answers `find` and `search` as its base does.
    #[test]
    fn seeded_layer_reads_dictionaries() {
        let p = based();
        let search = |c: ContextChoice, terms: &str| {
            let r = katalogr(&p.repo, c)
                .search(terms, 20, &Options::default())
                .unwrap();
            format!("{:?}", r.outcome)
        };
        let base_find = find(&p.repo, ContextChoice::Base, "Alpha");
        let base_search = search(ContextChoice::Base, "Alpha");
        assert!(base_find.starts_with("Found"), "{base_find}");
        assert!(base_search.contains("Alpha"), "{base_search}");
        // The first read of the current context seeds the layer.
        assert!(layers(&p).is_empty());
        assert_eq!(find(&p.repo, ContextChoice::Current, "Alpha"), base_find);
        assert_eq!(layers(&p).len(), 1);
        assert_eq!(search(ContextChoice::Current, "Alpha"), base_search);
    }

    #[test]
    fn overlay_holds_only_delta() {
        let p = based();
        find(&p.repo, ContextChoice::Current, "Alpha");
        let layer = layers(&p).remove(0);
        // Nothing differs from the base yet.
        assert_eq!(delta_count(&p, &layer), 0);
        std::fs::write(p.repo.join("c.tx"), "sym fn Gamma\nsym fn Delta\n").unwrap();
        ok(&p.repo, &["notify", "c.tx"]);
        assert!(has(&p.repo, ContextChoice::Current, "Delta"));
        assert_eq!(delta_count(&p, &layer), 1);
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

    #[test]
    fn schema_mismatch_reseeds() {
        use katalogr_store::{CACHE_SCHEMA, Cache};
        let p = based();
        assert!(has(&p.repo, ContextChoice::Current, "Alpha"));
        let layer = layers(&p).remove(0);
        // A cache of another schema and a leftover file: the layer is rebuilt from the
        // base, so the leftover is gone and the answer is the base's.
        {
            let dir = Katalogr::resolve(&p.repo, &Overrides::default())
                .unwrap()
                .project()
                .clone();
            let id = katalogr_store::ContextId::new(layer.file_name().unwrap().to_str().unwrap())
                .unwrap();
            let c = Cache::open(
                &dir.layer(&id),
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            )
            .unwrap();
            let mut tx = c.write().unwrap();
            tx.set_meta("cache_schema", &(CACHE_SCHEMA + 1).to_le_bytes())
                .unwrap();
            tx.commit().unwrap();
        }
        std::fs::write(layer.join("junk"), "x").unwrap();
        assert!(has(&p.repo, ContextChoice::Current, "Alpha"));
        assert!(!layer.join("junk").exists());
        assert_eq!(layers(&p).len(), 1);
    }

    /// A second checkout of the same repo, with its marker, as a worktree on a new branch.
    fn add_worktree(p: &P, name: &str) -> PathBuf {
        let wt = p.repo.with_file_name(name);
        git(
            &p.repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", name],
        );
        std::fs::copy(p.repo.join(".katalogr"), wt.join(".katalogr")).unwrap();
        wt
    }

    fn layer_of(wt: &Path) -> String {
        let canon = std::fs::canonicalize(wt).unwrap();
        katalogr_store::ContextId::for_checkout(camino::Utf8Path::from_path(&canon).unwrap())
            .to_string()
    }

    fn set_used(layer: &Path, secs_ago: u64) {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(layer.join(".used"))
            .unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago))
            .unwrap();
    }

    #[test]
    fn commit_context_reads_commit() {
        let p = based();
        let c1 = git(&p.repo, &["rev-parse", "HEAD"]);
        std::fs::write(p.repo.join("c.tx"), "sym fn GammaNew\n").unwrap();
        git(&p.repo, &["commit", "-qam", "c2"]);
        // The disk and the checkout move on; the commit context does not.
        std::fs::write(p.repo.join("c.tx"), "sym fn GammaDirty\n").unwrap();
        let k = katalogr(&p.repo, ContextChoice::Current);
        let info = k
            .context_add_commit("app", &c1, &Options::default())
            .unwrap();
        assert_eq!(info.kind, "commit");
        assert_eq!(info.commit.as_deref(), Some(c1.as_str()));
        let id = katalogr_store::ContextId::new(&info.id).unwrap();
        assert!(has(&p.repo, ContextChoice::Named(id.clone()), "Gamma"));
        assert!(!has(&p.repo, ContextChoice::Named(id.clone()), "GammaNew"));
        assert!(!has(&p.repo, ContextChoice::Named(id), "GammaDirty"));
        // Adding it again finds the same context.
        let again = k
            .context_add_commit("app", &c1[..10], &Options::default())
            .unwrap();
        assert_eq!(again.id, info.id);
        assert_eq!(
            k.context_list()
                .unwrap()
                .iter()
                .filter(|i| i.kind == "commit")
                .count(),
            1
        );
    }

    #[test]
    fn prune_removes_missing_roots_first() {
        let p = based();
        let (w1, w2) = (add_worktree(&p, "wt1"), add_worktree(&p, "wt2"));
        for at in [&p.repo, &w1, &w2] {
            assert!(has(at, ContextChoice::Current, "Alpha"));
        }
        let (id1, id2) = (layer_of(&w1), layer_of(&w2));
        let (l1, l2) = (layer_root(&p).join(&id1), layer_root(&p).join(&id2));
        // wt1 was used longest ago, wt2 more recently; but wt2's checkout is gone.
        set_used(&l1, 5000);
        set_used(&l2, 10);
        std::fs::remove_dir_all(&w2).unwrap();
        let k = katalogr(&p.repo, ContextChoice::Current);
        assert_eq!(k.context_list().unwrap().len(), 4);
        let removed = k.context_prune(2).unwrap();
        assert_eq!(
            removed.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
            [id2]
        );
        assert!(l1.exists() && !l2.exists());
        // Next the least recently used; never the current context.
        let removed = k.context_prune(0).unwrap();
        assert_eq!(
            removed.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
            [id1]
        );
        let left = k.context_list().unwrap();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(left.iter().any(|i| i.id == "base"));
        assert!(left.iter().any(|i| i.current));
    }

    #[test]
    fn prune_skips_locked() {
        use fs4::FileExt;
        let p = based();
        let (w1, w2) = (add_worktree(&p, "wt1"), add_worktree(&p, "wt2"));
        for at in [&p.repo, &w1, &w2] {
            assert!(has(at, ContextChoice::Current, "Alpha"));
        }
        let (l1, l2) = (
            layer_root(&p).join(layer_of(&w1)),
            layer_root(&p).join(layer_of(&w2)),
        );
        set_used(&l1, 9000);
        set_used(&l2, 100);
        // A process is using the oldest one.
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(l1.join(".cache/processor.lock"))
            .unwrap();
        FileExt::try_lock(&held).unwrap();
        let k = katalogr(&p.repo, ContextChoice::Current);
        let removed = k.context_prune(0).unwrap();
        assert_eq!(
            removed.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
            [layer_of(&w2)]
        );
        assert!(l1.exists() && !l2.exists());
    }

    #[test]
    fn seeding_past_contexts_max_removes_the_oldest() {
        let p = based();
        let w1 = add_worktree(&p, "wt1");
        assert!(has(&p.repo, ContextChoice::Current, "Alpha"));
        assert!(has(&w1, ContextChoice::Current, "Alpha"));
        let l_main = layers(&p)
            .into_iter()
            .find(|l| !l.ends_with(layer_of(&w1)))
            .unwrap();
        set_used(&l_main, 9000);
        // Room for two: a third checkout makes the oldest (not itself) go.
        let cfg = p.kx.join("projects/demo/config.yaml");
        let text = std::fs::read_to_string(&cfg).unwrap();
        let mut kept: Vec<&str> = text
            .lines()
            .filter(|l| !l.starts_with("settings:"))
            .collect();
        kept.push("settings:");
        kept.push("  contexts.max: 2");
        std::fs::write(&cfg, kept.join("\n") + "\n").unwrap();
        let w2 = add_worktree(&p, "wt2");
        assert!(has(&w2, ContextChoice::Current, "Alpha"));
        assert_eq!(layers(&p).len(), 2);
        assert!(!l_main.exists());
    }

    /// FEAT-080 with contexts: a read-only handle taken beside a writer opens the chosen
    /// context's own cache, not the base's.
    #[test]
    fn reader_opens_the_layer() {
        use katalogr_store::Cache;
        let p = based();
        std::fs::write(p.repo.join("c.tx"), "sym fn GammaLayer\n").unwrap();
        ok(&p.repo, &["notify", "c.tx"]);
        assert!(has(&p.repo, ContextChoice::Current, "GammaLayer"));
        let layer = layers(&p).remove(0);
        let dir = Katalogr::resolve(&p.repo, &Overrides::default())
            .unwrap()
            .project()
            .clone();
        let id =
            katalogr_store::ContextId::new(layer.file_name().unwrap().to_str().unwrap()).unwrap();
        // A writer holds the layer's cache, so the read has to take a read-only handle.
        let held = Cache::open(
            &dir.layer(&id),
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        let out = ok(
            &p.repo,
            &["--consistency", "stale-ok", "find", "GammaLayer"],
        );
        assert!(out.contains("GammaLayer"), "{out}");
        let (code, _, _) = run(
            &p.repo,
            &[
                "--context",
                "base",
                "--consistency",
                "stale-ok",
                "find",
                "GammaLayer",
            ],
        );
        assert_eq!(code, 4);
        drop(held);
    }

    /// FEAT-095 R-2. `context add --commit` that has not drained is reported, not a quiet
    /// success. Not timing-based: the context's processor lock is held, so it cannot drain.
    #[test]
    fn add_commit_reports_undrained() {
        use fs4::FileExt;
        let p = project(&FILES, true);
        let head = git(&p.repo, &["rev-parse", "HEAD"]);
        let k = katalogr(&p.repo, ContextChoice::Base);
        let quick = Options {
            timeout: Some(0.3),
            ..Options::default()
        };
        // Drained: the first add succeeds.
        k.context_add_commit("app", &head, &Options::default())
            .unwrap();
        let id = katalogr_store::ContextId::for_commit("app", &head);
        let layer = k.project().layer(&id);
        // Something is queued in the context, and a process holds its lock.
        katalogr_engine::queue::Inbox::new(&layer)
            .append(&katalogr_engine::NewEvent::rescan("app"))
            .unwrap();
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(layer.root().join(".cache/processor.lock"))
            .unwrap();
        FileExt::try_lock(&held).unwrap();
        let err = k
            .context_add_commit("app", &head, &quick)
            .expect_err("a context that has not drained is reported");
        assert_eq!(err.refused_exit(), Some(5), "{err}");
        drop(held);
        // Released, it drains.
        k.context_add_commit("app", &head, &Options::default())
            .unwrap();
    }
}
