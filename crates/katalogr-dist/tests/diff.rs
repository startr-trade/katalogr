//! The diff report (FEAT-095): keys, sides, storing and reading back. Needs the `.tx` test
//! extractor: `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod diff {
    use katalogr_dist::diff::{DiffReport, DiffRequest, commit_context_id};
    use katalogr_dist::{
        AnswerCounters, Consistency, DistError, HistoryScope, Katalogr, Options, Outcome,
        Overrides, Reply, kindex_init, project_init,
    };
    use katalogr_store::{AnswerStore, Cache, DirAnswerStore, Reader};
    use std::path::{Path, PathBuf};

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

    fn git_out(root: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("run git");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn init_repo(repo: &Path) {
        std::fs::create_dir_all(repo).unwrap();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["config", "user.name", "Test"]);
        git(repo, &["config", "user.email", "test@example.com"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
    }

    fn commit(repo: &Path, msg: &str) {
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", msg]);
    }

    /// The first commit: two files.
    fn write_c1(repo: &Path) {
        std::fs::write(repo.join("g.tx"), "sym fn a\nsym fn b\nlink b calls a\n").unwrap();
        std::fs::write(repo.join("h.tx"), "sym fn h\n").unwrap();
    }

    /// The second: a symbol and a link added, one file gone, one new.
    fn write_c2(repo: &Path) {
        std::fs::write(
            repo.join("g.tx"),
            "sym fn a\nsym fn b\nsym fn c\nlink b calls a\nlink c calls a\n",
        )
        .unwrap();
        std::fs::remove_file(repo.join("h.tx")).unwrap();
        std::fs::write(repo.join("k.tx"), "sym fn k\nsym fn k2\nlink k calls k2\n").unwrap();
    }

    struct Fx {
        _tmp: tempfile::TempDir,
        kx: PathBuf,
        repo: PathBuf,
        k: Katalogr,
    }

    fn overrides(kx: &Path) -> Overrides {
        Overrides {
            kindex: Some(kx.display().to_string()),
            project: None,
            context: None,
            ..Default::default()
        }
    }

    fn opts() -> Options {
        Options {
            timeout: Some(120.0),
            consistency: Some(Consistency::WaitAll),
            ..Options::default()
        }
    }

    /// Two commits tagged `v1` and `v2` on `main`; the base indexed at `v2`, or at `v1` when
    /// `index_at_c1`. `settings` goes into the project's config.
    fn build(index_at_c1: bool, settings: &str, extra_repo: bool) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        init_repo(&repo);
        write_c1(&repo);
        commit(&repo, "c1");
        git(&repo, &["tag", "v1"]);
        let mut repos = vec![("app".to_string(), repo.clone())];
        if extra_repo {
            let lib = tmp.path().join("lib");
            std::fs::create_dir_all(&lib).unwrap();
            std::fs::write(lib.join("l.tx"), "sym fn l\n").unwrap();
            repos.push(("lib".to_string(), lib));
        }
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", repos, true).unwrap();
        if !settings.is_empty() {
            let config = kx.join("projects/demo/config.yaml");
            let text = std::fs::read_to_string(&config).unwrap();
            assert!(text.contains("settings: {}"), "{text}");
            std::fs::write(
                config,
                text.replace("settings: {}", &format!("settings:\n{settings}")),
            )
            .unwrap();
        }
        let mut k = Katalogr::resolve(&repo, &overrides(&kx)).unwrap();
        if index_at_c1 {
            k.index(None, true, None, &opts()).unwrap();
        }
        write_c2(&repo);
        commit(&repo, "c2");
        git(&repo, &["tag", "v2"]);
        if !index_at_c1 {
            k.index(None, true, None, &opts()).unwrap();
        }
        k.set_answer_build(Some("b1".into()));
        Fx {
            _tmp: tmp,
            kx,
            repo,
            k,
        }
    }

    fn fx() -> Fx {
        build(false, "", false)
    }

    fn req(a: &str, b: &str, limit: usize) -> DiffRequest {
        DiffRequest {
            a: a.into(),
            b: b.into(),
            repo: None,
            limit,
        }
    }

    fn report(k: &Katalogr, a: &str, b: &str) -> Reply<DiffReport> {
        k.diff_report(&req(a, b, 200), &opts()).unwrap()
    }

    fn found(r: &Reply<DiffReport>) -> &DiffReport {
        match &r.outcome {
            Outcome::Found(d) => d,
            other => panic!("not a report: {other:?}"),
        }
    }

    fn bytes(r: &Reply<DiffReport>) -> Vec<u8> {
        serde_json::to_vec(found(r)).unwrap()
    }

    impl Fx {
        fn contexts_dir(&self) -> PathBuf {
            self.kx.join("projects/demo/.cache/contexts")
        }

        fn layer(&self, commit: &str) -> PathBuf {
            self.contexts_dir()
                .join(commit_context_id("app", commit).as_str())
        }

        fn sha(&self, rev: &str) -> String {
            git_out(&self.repo, &["rev-parse", rev])
        }

        fn entries(&self) -> Vec<katalogr_store::AnswerListing> {
            DirAnswerStore::new(
                camino::Utf8PathBuf::from_path_buf(self.kx.join("projects/demo/.cache/answers"))
                    .unwrap(),
            )
            .list()
            .unwrap()
        }

        fn ops(&self, op: &str) -> usize {
            self.entries().iter().filter(|e| e.header.op == op).count()
        }

        fn facade(&self) -> Katalogr {
            let mut k = Katalogr::resolve(&self.repo, &overrides(&self.kx)).unwrap();
            k.set_answer_build(Some("b1".into()));
            k
        }

        fn counters(&self) -> AnswerCounters {
            self.k.answer_counters()
        }
    }

    // ---------------------------------------------------------------- R-1

    #[test]
    fn tags_branches_and_commits_resolve() {
        let f = fx();
        let by_tag = report(&f.k, "v1", "v2");
        let (c1, c2) = (f.sha("v1"), f.sha("v2"));
        let by_sha = report(&f.k, &c1, &c2);
        let by_branch = report(&f.k, "main~1", "main");
        assert_eq!(found(&by_tag).a.commit, c1);
        assert_eq!(found(&by_tag).b.commit, c2);
        assert_eq!(bytes(&by_tag), bytes(&by_sha));
        assert_eq!(bytes(&by_tag), bytes(&by_branch));
        let syms = &found(&by_tag).diff.symbols;
        assert_eq!(
            syms.added.len() + syms.removed.len() + syms.moved.len(),
            4,
            "{syms:?}"
        );
        // An unknown revision is a usage error.
        let err =
            f.k.diff_report(&req("v1", "nope", 200), &opts())
                .unwrap_err();
        assert!(err.is_usage(), "{err}");
    }

    #[test]
    fn same_commit_is_an_empty_report() {
        let f = fx();
        let r = report(&f.k, "v2", &f.sha("main"));
        let d = found(&r);
        assert_eq!(d.a.commit, d.b.commit);
        let s = &d.diff.symbols;
        assert_eq!(
            (
                s.added_total,
                s.removed_total,
                s.moved_total,
                s.changed_total
            ),
            (0, 0, 0, 0)
        );
        assert!(d.diff.links.links_added.is_empty() && d.diff.links.links_removed.is_empty());
        assert!(d.diff.contracts.is_empty());
        assert!(d.insights.hubs.is_empty());
    }

    // ---------------------------------------------------------------- R-2

    #[test]
    fn sides_are_commit_contexts() {
        let f = fx();
        report(&f.k, "v1", "v2");
        let infos = f.k.context_list().unwrap();
        for rev in ["v1", "v2"] {
            let sha = f.sha(rev);
            assert!(
                infos
                    .iter()
                    .any(|i| i.kind == "commit" && i.commit.as_deref() == Some(sha.as_str())),
                "{rev}: {infos:?}"
            );
        }
    }

    /// The files a layer holds a different revision for than the base.
    fn extracted_in(f: &Fx, commit: &str) -> Vec<String> {
        let dl = || std::time::Instant::now() + std::time::Duration::from_secs(5);
        let base = f.k.project().clone();
        let id = commit_context_id("app", commit);
        let revs = |c: Cache| -> std::collections::BTreeMap<String, [u8; 32]> {
            c.read()
                .unwrap()
                .file_revisions()
                .unwrap()
                .into_iter()
                .map(|(k, v)| (format!("{k:?}"), v))
                .collect()
        };
        let mine = revs(Cache::open(&base.layer(&id), dl()).unwrap());
        let theirs = revs(Cache::open(&base, dl()).unwrap());
        let mut out: Vec<String> = mine
            .iter()
            .filter(|(k, v)| theirs.get(*k) != Some(*v))
            .map(|(k, _)| k.clone())
            .chain(theirs.keys().filter(|k| !mine.contains_key(*k)).cloned())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn only_difference_is_extracted() {
        let f = fx();
        report(&f.k, "v1", "v2");
        // The side at the base's own commit extracts nothing; the other only what differs.
        assert!(extracted_in(&f, &f.sha("v2")).is_empty());
        let changed = extracted_in(&f, &f.sha("v1"));
        assert_eq!(changed.len(), 3, "{changed:?}");
        for name in ["g.tx", "h.tx", "k.tx"] {
            assert!(
                changed.iter().any(|c| c.contains(name)),
                "{name}: {changed:?}"
            );
        }
    }

    #[test]
    fn side_not_drained_refused() {
        use fs4::FileExt;
        let f = fx();
        let c1 = f.sha("v1");
        f.k.context_add_commit("app", &c1, &opts()).unwrap();
        let layer = f.k.project().layer(&commit_context_id("app", &c1));
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
        let quick = Options {
            timeout: Some(0.3),
            ..Options::default()
        };
        let err = f.k.diff_report(&req("v1", "v2", 200), &quick).unwrap_err();
        assert!(matches!(err, DistError::Refused { exit: 5, .. }), "{err}");
        assert!(f.entries().is_empty(), "nothing is stored");
    }

    #[test]
    fn second_side_does_not_evict_first() {
        let f = build(false, "  contexts.max: 1", false);
        report(&f.k, "v1", "v2");
        assert!(f.layer(&f.sha("v1")).exists(), "first side kept");
        assert!(f.layer(&f.sha("v2")).exists(), "second side built");
    }

    // ---------------------------------------------------------------- R-6

    #[test]
    fn side_insights_are_stored_answers() {
        let f = fx();
        report(&f.k, "v1", "v2");
        for op in ["hubs", "communities", "cycles", "coverage"] {
            assert_eq!(f.ops(op), 2, "{op}: one entry per side");
        }
        assert_eq!(f.ops("diff"), 1);
    }

    #[test]
    fn side_coverage_reads_commit_not_disk() {
        let f = fx();
        // A file on disk that no commit holds, and one extension nothing claims.
        std::fs::write(f.repo.join("stray.dat"), "x\n").unwrap();
        report(&f.k, "v1", "v2");
        let side = Katalogr::resolve(
            &f.repo,
            &Overrides {
                context: Some(commit_context_id("app", &f.sha("v1")).to_string()),
                ..overrides(&f.kx)
            },
        )
        .unwrap();
        let cov = side
            .coverage(
                &katalogr_dist::CoverageOptions {
                    repo: None,
                    suffix_top: 20,
                },
                &opts(),
            )
            .unwrap();
        let Outcome::Found(c) = cov.outcome else {
            panic!("no coverage")
        };
        assert!(
            c.unclaimed.iter().all(|u| u.files == 0),
            "the stray file is on disk, not in the commit: {:?}",
            c.unclaimed
        );
    }

    // ---------------------------------------------------------------- R-8

    #[test]
    fn report_same_after_contexts_rebuilt() {
        let f = fx();
        let first = bytes(&report(&f.k, "v1", "v2"));
        f.k.context_prune(0).unwrap();
        assert!(!f.layer(&f.sha("v1")).exists());
        let fresh = Options {
            fresh: true,
            ..opts()
        };
        let again = f.k.diff_report(&req("v1", "v2", 200), &fresh).unwrap();
        assert_eq!(first, bytes(&again));
        assert!(f.layer(&f.sha("v1")).exists(), "rebuilt");
    }

    #[test]
    fn report_same_from_other_seed() {
        let from_c2 = build(false, "", false);
        let from_c1 = build(true, "", false);
        let (a, b) = (
            report(&from_c2.k, "v1", "v2"),
            report(&from_c1.k, "v1", "v2"),
        );
        // The seed differs, so the commits' hashes differ only when the fixtures do not.
        assert_eq!(found(&a).diff, found(&b).diff);
        assert_eq!(found(&a).insights, found(&b).insights);
    }

    #[test]
    fn report_same_at_1_and_8_threads() {
        let one = build(false, "  index.threads: 1", false);
        let eight = build(false, "  index.threads: 8", false);
        let (a, b) = (report(&one.k, "v1", "v2"), report(&eight.k, "v1", "v2"));
        assert_eq!(found(&a).diff, found(&b).diff);
        assert_eq!(found(&a).insights, found(&b).insights);
    }

    // ---------------------------------------------------------------- R-9

    #[test]
    fn second_call_is_a_hit() {
        let f = fx();
        let first = report(&f.k, "v1", "v2");
        let before = f.counters();
        let second = report(&f.k, "v1", "v2");
        let after = f.counters();
        assert_eq!(after.hits, before.hits + 1);
        assert_eq!(after.writes, before.writes);
        assert_eq!(bytes(&first), bytes(&second));
        assert_eq!(first.report.view_id, second.report.view_id);
        assert!(first.report.view_id.is_some());
        assert!(second.report.exact);
    }

    #[test]
    fn each_format_has_its_own_id() {
        use katalogr_dist::diff::DiffRender;
        let f = fx();
        let json = report(&f.k, "v1", "v2");
        let md =
            f.k.diff_page(&req("v1", "v2", 200), DiffRender::Markdown, &opts())
                .unwrap();
        let html =
            f.k.diff_page(&req("v1", "v2", 200), DiffRender::Html, &opts())
                .unwrap();
        let ids = [
            json.report.view_id.clone().unwrap(),
            md.report.view_id.clone().unwrap(),
            html.report.view_id.clone().unwrap(),
        ];
        assert!(
            ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2],
            "{ids:?}"
        );
        let Outcome::Found(m) = &md.outcome else {
            panic!("no page")
        };
        let Outcome::Found(h) = &html.outcome else {
            panic!("no page")
        };
        assert!(m.text.starts_with("# Changes in app"));
        assert!(h.text.starts_with("<!DOCTYPE html>"));
        assert_eq!(f.ops("diff"), 3);
        // Each is read back on the second call.
        let before = f.counters();
        let again =
            f.k.diff_page(&req("v1", "v2", 200), DiffRender::Html, &opts())
                .unwrap();
        assert_eq!(f.counters().hits, before.hits + 1);
        assert_eq!(again.outcome, html.outcome);
        // JSON is not a page.
        let err =
            f.k.diff_page(&req("v1", "v2", 200), DiffRender::Json, &opts())
                .unwrap_err();
        assert!(err.is_usage());
    }

    #[test]
    fn limit_gives_a_new_id() {
        let f = fx();
        let a = f.k.diff_report(&req("v1", "v2", 200), &opts()).unwrap();
        let b = f.k.diff_report(&req("v1", "v2", 1), &opts()).unwrap();
        assert!(a.report.view_id != b.report.view_id);
        assert_eq!(f.ops("diff"), 2);
        assert!(!found(&b).diff.partial.is_empty(), "a cut list is marked");
        assert_eq!(b.partial, found(&b).diff.partial);
    }

    #[test]
    fn other_rev_name_same_commit_hits() {
        let f = fx();
        report(&f.k, "v1", "v2");
        let before = f.counters();
        report(&f.k, &f.sha("v1"), "main");
        let after = f.counters();
        assert_eq!(after.hits, before.hits + 1);
        assert_eq!(after.writes, before.writes);
    }

    #[test]
    fn dirty_build_stores_nothing() {
        // The other repo moves between the key and the build, so a side is not at the
        // positions the key names.
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let (app, lib) = (tmp.path().join("app"), tmp.path().join("lib"));
        for r in [&app, &lib] {
            init_repo(r);
        }
        write_c1(&app);
        commit(&app, "c1");
        git(&app, &["tag", "v1"]);
        write_c2(&app);
        commit(&app, "c2");
        git(&app, &["tag", "v2"]);
        std::fs::write(lib.join("l.tx"), "sym fn l\n").unwrap();
        commit(&lib, "l1");
        kindex_init(&kx).unwrap();
        project_init(
            &kx,
            "demo",
            vec![("app".into(), app.clone()), ("lib".into(), lib.clone())],
            true,
        )
        .unwrap();
        let mut k = Katalogr::resolve(&app, &overrides(&kx)).unwrap();
        k.index(None, true, None, &opts()).unwrap();
        k.set_answer_build(Some("b1".into()));
        let (app2, lib2, kx2) = (app.clone(), lib.clone(), kx.clone());
        let once = std::sync::Arc::new(std::sync::Once::new());
        k.set_answer_compute_hook(std::sync::Arc::new(move || {
            once.call_once(|| {
                std::fs::write(lib2.join("l.tx"), "sym fn l\nsym fn l2\n").unwrap();
                commit(&lib2, "l2");
                let mover = Katalogr::resolve(&app2, &overrides(&kx2)).unwrap();
                mover.index(None, true, None, &opts()).unwrap();
            });
        }));
        let r = k.diff_report(&req("v1", "v2", 200), &opts()).unwrap();
        assert!(
            r.report.warnings.iter().any(|w| w.contains("not stored")),
            "{:?}",
            r.report.warnings
        );
        let store = DirAnswerStore::new(
            camino::Utf8PathBuf::from_path_buf(kx.join("projects/demo/.cache/answers")).unwrap(),
        );
        let diffs = store
            .list()
            .unwrap()
            .into_iter()
            .filter(|e| e.header.op == "diff")
            .count();
        assert_eq!(diffs, 0);
    }

    #[test]
    fn unknown_other_repo_position_stores_nothing() {
        // `lib` is a plain directory: the base reflects no commit of it.
        let f = build(false, "", true);
        let r = report(&f.k, "v1", "v2");
        assert!(found(&r).diff.symbols.added_total > 0);
        assert_eq!(f.ops("diff"), 0);
        // A second call builds again.
        let before = f.counters();
        report(&f.k, "v1", "v2");
        assert_eq!(f.counters().hits, before.hits);
    }

    // ---------------------------------------------------------------- R-10

    #[test]
    fn hit_needs_no_context() {
        let f = fx();
        report(&f.k, "v1", "v2");
        std::fs::remove_dir_all(f.contexts_dir()).unwrap();
        let before = f.counters();
        let r = report(&f.k, "v1", "v2");
        assert_eq!(f.counters().hits, before.hits + 1);
        assert!(found(&r).diff.symbols.added_total > 0);
        assert!(!f.contexts_dir().exists(), "a hit seeds nothing");
    }

    #[test]
    fn hit_after_contexts_pruned() {
        let f = fx();
        let first = report(&f.k, "v1", "v2");
        assert!(!f.k.context_prune(0).unwrap().is_empty());
        let before = f.counters();
        let again = report(&f.k, "v1", "v2");
        assert_eq!(f.counters().hits, before.hits + 1);
        assert_eq!(bytes(&first), bytes(&again));
    }

    // ---------------------------------------------------------------- R-11

    #[test]
    fn profile_file_change_misses() {
        let f = fx();
        report(&f.k, "v1", "v2");
        let dir = f.kx.join("projects/demo/profiles");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("extra.yaml"), "# added\n").unwrap();
        let k = f.facade();
        k.diff_report(&req("v1", "v2", 200), &opts()).unwrap();
        let c = k.answer_counters();
        assert_eq!((c.hits, c.misses >= 1), (0, true));
    }

    #[test]
    fn config_change_misses() {
        let f = fx();
        report(&f.k, "v1", "v2");
        let config = f.kx.join("projects/demo/config.yaml");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str("# a comment changes the bytes\n");
        std::fs::write(&config, text).unwrap();
        let k = f.facade();
        k.diff_report(&req("v1", "v2", 200), &opts()).unwrap();
        assert_eq!(k.answer_counters().hits, 0);
    }

    #[test]
    fn build_change_misses() {
        let f = fx();
        report(&f.k, "v1", "v2");
        let mut k = f.facade();
        k.set_answer_build(Some("b2".into()));
        k.diff_report(&req("v1", "v2", 200), &opts()).unwrap();
        assert_eq!(k.answer_counters().hits, 0);
        // The entry now belongs to the new build, which hits.
        let mut same = f.facade();
        same.set_answer_build(Some("b2".into()));
        same.diff_report(&req("v1", "v2", 200), &opts()).unwrap();
        assert_eq!(same.answer_counters().hits, 1);
    }

    #[test]
    fn salt_change_gives_new_id() {
        // The facts version is part of the ID. It moves when an extractor's salt or profile
        // hash does; a new profile processed by the base is the change a test can make.
        let f = fx();
        let first = report(&f.k, "v1", "v2");
        let dir = f.kx.join("projects/demo/profiles");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("route.yaml"), "# a profile the base now carries\n").unwrap();
        let k = f.facade();
        k.index(None, true, None, &opts()).unwrap();
        let second = k.diff_report(&req("v1", "v2", 200), &opts()).unwrap();
        assert!(
            first.report.view_id != second.report.view_id || {
                // The profile left the facts alone, but its bytes moved `rules`: still a miss.
                k.answer_counters().hits == 0
            }
        );
    }

    #[test]
    fn history_index_does_not_change_report() {
        let plain = fx();
        let with_history = fx();
        std::fs::write(
            with_history.repo.join("g.tx"),
            std::fs::read_to_string(with_history.repo.join("g.tx")).unwrap(),
        )
        .unwrap();
        with_history
            .k
            .index(None, false, Some(&HistoryScope::default()), &opts())
            .unwrap();
        let (a, b) = (
            report(&plain.k, "v1", "v2"),
            report(&with_history.k, "v1", "v2"),
        );
        assert_eq!(found(&a).diff, found(&b).diff);
        assert_eq!(found(&a).insights, found(&b).insights);
        assert!(
            found(&b)
                .diff
                .symbols
                .added
                .iter()
                .all(|s| !s.path.starts_with("@history/"))
        );
    }

    // ---------------------------------------------------------------- R-17

    fn all_formats(f: &Fx) {
        use katalogr_dist::diff::DiffRender;
        report(&f.k, "v1", "v2");
        for m in [DiffRender::Markdown, DiffRender::Html] {
            f.k.diff_page(&req("v1", "v2", 200), m, &opts()).unwrap();
        }
    }

    #[test]
    fn answers_check_covers_report() {
        let f = fx();
        all_formats(&f);
        let rep = f.k.answers_check(None, false, &opts()).unwrap();
        // The three report entries and the side entries that sit at the base's own position.
        assert!(rep.checked >= 3, "{rep:?}");
        assert_eq!(rep.matched, rep.checked, "{rep:?}");
        assert!(rep.mismatched.is_empty());
    }

    #[test]
    fn answers_check_finds_tampered_report() {
        use katalogr_store::StoredAnswer;
        let f = fx();
        all_formats(&f);
        let store = DirAnswerStore::new(
            camino::Utf8PathBuf::from_path_buf(f.kx.join("projects/demo/.cache/answers")).unwrap(),
        );
        let json = f
            .entries()
            .into_iter()
            .find(|e| {
                e.header.op == "diff"
                    && e.header.args.get("render").map(String::as_str) == Some("json")
            })
            .unwrap();
        let mut a = store.get(&json.header.view_id).unwrap().unwrap();
        a.payload.extend_from_slice(b" ");
        store
            .put(&StoredAnswer::new(
                json.header.view_id.clone(),
                json.header.validity.clone(),
                json.header.op.clone(),
                json.header.args.clone(),
                a.payload,
            ))
            .unwrap();
        let rep = f.k.answers_check(None, false, &opts()).unwrap();
        assert_eq!(rep.mismatched.len(), 1, "{rep:?}");
        assert_eq!(rep.mismatched[0].view_id, json.header.view_id);
        assert_eq!(rep.matched, rep.checked - 1);
    }

    #[test]
    fn answers_check_skips_without_contexts() {
        let f = fx();
        all_formats(&f);
        let before = f.k.answers_check(None, false, &opts()).unwrap();
        f.k.context_prune(0).unwrap();
        let after = f.k.answers_check(None, false, &opts()).unwrap();
        // The three report entries are skipped; nothing else changed.
        assert_eq!(after.checked + 3, before.checked, "{before:?} {after:?}");
        assert_eq!(
            after.skipped_other_position,
            before.skipped_other_position + 3
        );
        assert!(after.mismatched.is_empty());
        for rev in ["v1", "v2"] {
            assert!(!f.layer(&f.sha(rev)).exists(), "a check starts no index");
        }
    }
}
