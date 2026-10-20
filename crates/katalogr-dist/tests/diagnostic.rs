//! FEAT-065: the facade returns a trace only when asked, and the answer is the same either way.
//! Needs the `.tx` test extractor: `cargo test -p katalogr-dist --features test-extractor`
//! (a workspace test run enables it through the CLI crate's dev-dependency).
#![cfg(feature = "test-extractor")]

mod diagnostic {
    use katalogr_dist::{Katalogr, Options, Outcome, Overrides, kindex_init, project_init};

    #[test]
    fn reply_carries_trace_only_when_asked() {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("a.tx"),
            "sym fn target\nsym fn caller_a\nlink caller_a calls target\n",
        )
        .unwrap();
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        let k = Katalogr::resolve(
            &repo,
            &Overrides {
                kindex: Some(kx.display().to_string()),
                project: None,
            },
        )
        .unwrap();
        k.index(None, false, None, &Options::default()).unwrap();

        let plain = Options::default();
        let asked = Options {
            diagnostic: true,
            ..Options::default()
        };
        let a = k.search("caller", 20, &plain).unwrap();
        let b = k.search("caller", 20, &asked).unwrap();
        assert!(a.diagnostic.is_none());
        assert!(b.diagnostic.as_ref().is_some_and(|t| t.routing.is_some()));
        assert_eq!(a.outcome, b.outcome);

        let a = k.refs("target", &plain).unwrap();
        let b = k.refs("target", &asked).unwrap();
        assert!(a.diagnostic.is_none());
        assert!(b.diagnostic.as_ref().is_some_and(|t| !t.weights.is_empty()));
        assert_eq!(a.outcome, b.outcome);
        assert!(matches!(b.outcome, Outcome::Found(_)));

        let a = k.map(&plain).unwrap();
        let b = k.map(&asked).unwrap();
        assert!(a.diagnostic.is_none());
        assert!(b.diagnostic.is_some_and(|t| t.rank_source.is_some()));
        assert_eq!(a.outcome, b.outcome);

        // Commands that do not trace never carry one, even when asked.
        let c = k.find("target", &asked).unwrap();
        assert!(c.diagnostic.is_none());
    }
}
