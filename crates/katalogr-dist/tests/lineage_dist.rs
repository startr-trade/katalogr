//! FEAT-101 T4: `Katalogr::lineage` over a synthetic `.tx` repo. Needs the test extractor:
//! `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod lineage_dist {
    use katalogr_dist::query::{LineageOptions, LineageReport, ViewQuery};
    use katalogr_dist::{
        Consistency, Katalogr, Options, Outcome, Overrides, kindex_init, project_init,
    };
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

    /// `B` calls `A`.
    const CODE: &str = "sym fn A\nsym fn B\nlink B calls A\n";

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

    fn view() -> Options {
        Options {
            view: Some(ViewQuery {
                op: "lineage".into(),
                args: Default::default(),
            }),
            ..Options::default()
        }
    }

    fn ask(k: &Katalogr, text: &str, opts: &Options) -> katalogr_dist::Reply<LineageReport> {
        k.lineage(text, LineageOptions::default(), opts).unwrap()
    }

    #[test]
    fn answer_names_its_project() {
        let w = world();
        let k = open(&w);
        let r = ask(&k, "A", &Options::default());
        let Outcome::Found(rep) = &r.outcome else {
            panic!("{:?}", r.outcome)
        };
        assert_eq!(rep.projects, ["demo"]);
        assert_eq!(rep.subject.name, "app:code.tx#fn:A");
        assert_eq!(r.partial, rep.partial);
        // A name no symbol has passes through as no match.
        let none = ask(&k, "Nope", &Options::default());
        assert!(matches!(none.outcome, Outcome::NoMatch(_)));
    }

    #[test]
    fn exact_answer_has_view_id() {
        let w = world();
        let k = open(&w);
        let a = ask(&k, "A", &view());
        assert!(a.report.exact, "{:?}", a.report);
        assert!(a.report.view_id.is_some());
        let b = ask(&k, "A", &view());
        assert_eq!(a.report.view_id, b.report.view_id);
    }

    #[test]
    fn stale_answer_marked() {
        let w = world();
        let k = open(&w);
        k.notify(&["code.tx".to_string()], false).unwrap();
        let o = Options {
            consistency: Some(Consistency::StaleOk),
            ..view()
        };
        let r = ask(&k, "A", &o);
        assert!(!r.report.exact);
        assert!(r.report.stale);
        assert!(r.report.view_id.is_none());
    }

    #[test]
    fn position_unavailable() {
        let w = world();
        let k = open(&w);
        let held = ask(&k, "A", &Options::default()).report.positions[0].text();
        std::fs::write(w.app.join("code.tx"), "sym fn A\n").unwrap();
        git(&w.app, &["commit", "-q", "-am", "edit"]);
        k.index(None, true, None, &Options::default()).unwrap();
        // Nothing holds the first position any more.
        let o = Options {
            at: Some(held),
            ..Options::default()
        };
        let r = ask(&k, "A", &o);
        assert!(
            matches!(r.outcome, Outcome::PositionUnavailable { .. }),
            "{:?}",
            r.outcome
        );
    }
}
