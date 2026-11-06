//! FEAT-028 T5: `Katalogr::impact` over a synthetic `.tx` repo. Needs the test extractor:
//! `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod impact_dist {
    use katalogr_dist::query::{ImpactOptions, ImpactReport};
    use katalogr_dist::{
        DistError, ImpactInput, Katalogr, Options, Outcome, Overrides, ViewQuery, kindex_init,
        project_init,
    };
    use std::path::{Path, PathBuf};

    fn git(dir: &Path, args: &[&str]) -> String {
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
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// `B` calls `A`, `C` calls `B`.
    const CODE: &str = "sym fn A\nsym fn B\nsym fn C\nlink B calls A\nlink C calls B\n";

    struct World {
        _tmp: tempfile::TempDir,
        app: PathBuf,
    }

    fn world() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let app = root.join("app");
        std::fs::create_dir_all(&app).unwrap();
        git(&app, &["init", "-q", "-b", "main"]);
        std::fs::write(app.join("code.tx"), CODE).unwrap();
        std::fs::write(app.join(".gitignore"), ".katalogr\n").unwrap();
        git(&app, &["add", "."]);
        git(&app, &["commit", "-q", "-m", "base"]);
        let kx = root.join("kx");
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".to_string(), app.clone())], true).unwrap();
        World { _tmp: tmp, app }
    }

    fn open(w: &World) -> Katalogr {
        let k = Katalogr::resolve(&w.app, &Overrides::default()).unwrap();
        k.index(None, true, None, &Options::default()).unwrap();
        k
    }

    fn edit(w: &World, from: &str, to: &str) {
        let t = std::fs::read_to_string(w.app.join("code.tx")).unwrap();
        std::fs::write(w.app.join("code.tx"), t.replace(from, to)).unwrap();
    }

    fn commit_edit(w: &World, from: &str, to: &str) {
        edit(w, from, to);
        git(&w.app, &["commit", "-q", "-am", "edit"]);
    }

    fn ask(
        k: &Katalogr,
        input: &ImpactInput<'_>,
        opts: &Options,
    ) -> Result<katalogr_dist::Reply<ImpactReport>, DistError> {
        k.impact(input, &ImpactOptions::default(), opts)
    }

    fn found(k: &Katalogr, input: &ImpactInput<'_>, opts: &Options) -> ImpactReport {
        match ask(k, input, opts).unwrap().outcome {
            Outcome::Found(r) => r,
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    fn hit_names(r: &ImpactReport) -> Vec<String> {
        let mut v: Vec<String> = r
            .groups
            .iter()
            .flat_map(|g| g.hits.iter().map(|h| h.node.name.clone()))
            .collect();
        v.sort();
        v
    }

    fn view() -> Options {
        Options {
            view: Some(ViewQuery {
                op: "impact".into(),
                args: Default::default(),
            }),
            ..Options::default()
        }
    }

    #[test]
    fn file_seeds_artifact_and_symbols() {
        let w = world();
        let k = open(&w);
        for text in ["code.tx", "app:code.tx"] {
            let r = found(&k, &ImpactInput::File(text), &Options::default());
            assert_eq!(r.seeds.len(), 4, "{text}: {:?}", r.seeds);
            assert!(r.seeds.iter().all(|s| s.precision == "file"));
        }
    }

    #[test]
    fn unknown_file_is_no_match() {
        let w = world();
        let k = open(&w);
        let r = ask(&k, &ImpactInput::File("nope.tx"), &Options::default()).unwrap();
        assert!(matches!(r.outcome, Outcome::NoMatch(_)), "{:?}", r.outcome);
    }

    #[test]
    fn range_matches_changed_lines() {
        let w = world();
        commit_edit(&w, "sym fn A", "sym  fn A");
        let k = open(&w);
        let r = found(&k, &ImpactInput::Range("HEAD~1..HEAD"), &Options::default());
        assert_eq!(r.seeds.len(), 1, "{:?}", r.seeds);
        assert_eq!(r.seeds[0].node.name, "app:code.tx#fn:A");
        assert_eq!(r.seeds[0].precision, "symbol");
        assert_eq!(hit_names(&r), ["app:code.tx#fn:B", "app:code.tx#fn:C"]);
        // A usage error for text that is not a range.
        let e = ask(&k, &ImpactInput::Range("HEAD"), &Options::default())
            .err()
            .unwrap();
        assert!(e.is_usage(), "{e}");
        let e = ask(&k, &ImpactInput::Range("a...b"), &Options::default())
            .err()
            .unwrap();
        assert!(e.is_usage(), "{e}");
    }

    #[test]
    fn no_seed_reads_working_tree_changes() {
        let w = world();
        git(&w.app, &["checkout", "-q", "-b", "feat"]);
        edit(&w, "sym fn B", "sym  fn B");
        let k = open(&w);
        let r = found(&k, &ImpactInput::Since(None), &Options::default());
        assert_eq!(r.seeds.len(), 1, "{:?}", r.seeds);
        assert_eq!(r.seeds[0].node.name, "app:code.tx#fn:B");
        assert_eq!(hit_names(&r), ["app:code.tx#fn:C"]);
    }

    #[test]
    fn range_head_not_held_is_unavailable() {
        let w = world();
        commit_edit(&w, "sym fn A", "sym  fn A");
        commit_edit(&w, "sym fn B", "sym  fn B");
        let k = open(&w);
        // HEAD~1 was never indexed, and is not this checkout's position.
        let r = ask(
            &k,
            &ImpactInput::Range("HEAD~2..HEAD~1"),
            &Options::default(),
        )
        .unwrap();
        assert!(
            matches!(r.outcome, Outcome::PositionUnavailable { .. }),
            "{:?}",
            r.outcome
        );
    }

    #[test]
    fn range_base_is_in_view_id() {
        let w = world();
        commit_edit(&w, "sym fn A", "sym  fn A");
        commit_edit(&w, "sym fn B", "sym  fn B");
        let k = open(&w);
        // The same positions, a different base: the IDs differ.
        let a = ask(&k, &ImpactInput::Since(Some("HEAD~1")), &view()).unwrap();
        let b = ask(&k, &ImpactInput::Since(Some("HEAD~2")), &view()).unwrap();
        assert!(a.report.exact && b.report.exact);
        assert_eq!(a.report.positions, b.report.positions);
        assert!(a.report.view_id.is_some());
        assert_ne!(a.report.view_id, b.report.view_id);
        // And the same question repeats its ID.
        let c = ask(&k, &ImpactInput::Since(Some("HEAD~1")), &view()).unwrap();
        assert_eq!(a.report.view_id, c.report.view_id);
    }

    #[test]
    fn failed_range_leaves_no_extra() {
        let w = world();
        commit_edit(&w, "sym fn A", "sym  fn A");
        let k = open(&w);
        let fresh = ask(&k, &ImpactInput::Symbol("A"), &view()).unwrap();
        assert!(fresh.report.view_id.is_some());
        // A range whose head is unknown fails after the base commit is known... and one whose
        // head is not held ends the read before the report is finished.
        let e = ask(&k, &ImpactInput::Range("HEAD~1..no-such-rev"), &view());
        assert!(e.is_err());
        let r = ask(&k, &ImpactInput::Range("HEAD~1..HEAD~1"), &view());
        drop(r);
        let again = ask(&k, &ImpactInput::Symbol("A"), &view()).unwrap();
        assert_eq!(fresh.report.view_id, again.report.view_id);
    }

    #[test]
    fn unknown_item_is_no_match() {
        let w = world();
        let k = open(&w);
        let r = ask(&k, &ImpactInput::Item("FEAT-999"), &Options::default()).unwrap();
        assert!(matches!(r.outcome, Outcome::NoMatch(_)), "{:?}", r.outcome);
    }

    #[test]
    fn answer_names_its_project() {
        let w = world();
        let k = open(&w);
        let r = ask(&k, &ImpactInput::Symbol("A"), &Options::default()).unwrap();
        let Outcome::Found(rep) = &r.outcome else {
            panic!("{:?}", r.outcome)
        };
        assert_eq!(rep.projects, ["demo"]);
        assert_eq!(hit_names(rep), ["app:code.tx#fn:B", "app:code.tx#fn:C"]);
        assert_eq!(r.partial, rep.partial);
    }
}
