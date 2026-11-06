//! FEAT-043: planning and rescanning inside a context layer.
mod support;

mod context {
    use super::support::{CountedExtractor, drained_graph, init_project};
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_engine::registry::ExtensionRegistry;
    use katalogr_engine::{Engine, EventKind, NewEvent};
    use katalogr_model::RelPath;
    use katalogr_store::{
        ContextId, ContextKind, ContextManifest, Kindex, ProjectDir, ProjectName, Reader,
        seed_layer,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn dl() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    fn base_dir(kx: &Utf8Path) -> ProjectDir {
        Kindex::open(kx)
            .unwrap()
            .project(&ProjectName::new("p").unwrap())
            .unwrap()
    }

    fn engine(dir: ProjectDir, work: &Utf8Path) -> (Engine, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(CountedExtractor(n.clone())));
        let roots = BTreeMap::from([("r".to_string(), work.to_path_buf())]);
        (Engine::open(dir, reg, roots, dl()).unwrap(), n)
    }

    struct S {
        _tmp: tempfile::TempDir,
        kx: Utf8PathBuf,
        work: Utf8PathBuf,
    }

    /// A base indexed from `files`.
    fn setup(files: &[(&str, &str)]) -> S {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let (kx, work) = (root.join("kx"), root.join("work"));
        std::fs::create_dir_all(&work).unwrap();
        for (n, t) in files {
            std::fs::write(work.join(n), t).unwrap();
        }
        init_project(&kx);
        let (e, _) = engine(base_dir(&kx), &work);
        e.inbox().append(&NewEvent::rescan("r")).unwrap();
        drained_graph(&e);
        S {
            _tmp: tmp,
            kx,
            work,
        }
    }

    /// Seed a layer from the base and open an engine on it.
    fn layer(s: &S) -> (Engine, Arc<AtomicUsize>) {
        let base = base_dir(&s.kx);
        let id = ContextId::new("w-test").unwrap();
        let l = base.layer(&id);
        let m = ContextManifest {
            format: 1,
            id,
            kind: ContextKind::Checkout,
            repo: "r".into(),
            root: None,
            seed_commit: None,
            commit: None,
        };
        seed_layer(&base, &l, &m, dl()).unwrap();
        engine(l, &s.work)
    }

    fn rescan(e: &Engine) -> String {
        e.inbox().append(&NewEvent::rescan("r")).unwrap();
        drained_graph(e)
    }

    fn changed(e: &Engine, p: &str, kind: EventKind) -> String {
        e.inbox()
            .append(&NewEvent::file("r", &RelPath::new(p).unwrap(), kind))
            .unwrap();
        drained_graph(e)
    }

    #[test]
    fn rescan_in_layer_extracts_nothing_unchanged() {
        let s = setup(&[("a.tx", "sym fn A\n"), ("b.tx", "sym fn B\n")]);
        let (e, n) = layer(&s);
        let before = drained_graph(&e);
        assert_eq!(rescan(&e), before);
        assert_eq!(n.load(Ordering::SeqCst), 0);
        let p = RelPath::new("b.tx").unwrap();
        let seeded = e.cache().read().unwrap().file_revision("r", &p).unwrap();
        assert!(seeded.is_some(), "the seed keeps the file revisions");
        // One edit: one extraction and a new revision, and a rescan then does nothing more.
        std::fs::write(s.work.join("b.tx"), "sym fn B\nsym fn B2\n").unwrap();
        let g = changed(&e, "b.tx", EventKind::Changed);
        assert!(g.contains("B2"));
        assert_eq!(n.load(Ordering::SeqCst), 1);
        assert_eq!(rescan(&e), g);
        assert_eq!(n.load(Ordering::SeqCst), 1);
        let now = e.cache().read().unwrap().file_revision("r", &p).unwrap();
        assert!(
            now.is_some() && now != seeded,
            "the edit changed the revision"
        );
    }

    #[test]
    fn deleted_base_file_leaves_context() {
        let s = setup(&[("a.tx", "sym fn A\n"), ("b.tx", "sym fn B\n")]);
        let (e, _) = layer(&s);
        assert!(drained_graph(&e).contains("sym"));
        std::fs::remove_file(s.work.join("b.tx")).unwrap();
        let g = changed(&e, "b.tx", EventKind::Deleted);
        assert!(!g.contains("b.tx") && g.contains("a.tx"), "{g}");
        drop(e);
        // A rescan agrees, and the base still has the file.
        let (e, _) = engine(
            base_dir(&s.kx).layer(&ContextId::new("w-test").unwrap()),
            &s.work,
        );
        assert!(!rescan(&e).contains("b.tx"));
        drop(e);
        let (b, _) = engine(base_dir(&s.kx), &s.work);
        let g = b
            .with_consistency(
                &katalogr_engine::QueryOptions {
                    consistency: katalogr_engine::Consistency::StaleOk,
                    timeout: Duration::from_secs(5),
                    scope: katalogr_engine::Scope::Project,
                },
                |tx| katalogr_engine::canonical_graph(tx),
            )
            .unwrap()
            .value;
        assert!(g.contains("b.tx"), "{g}");
    }

    /// R-3. The index errors of the base come with the copy, and an unchanged file keeps them
    /// without being extracted again.
    #[test]
    fn seeded_errors_kept_for_unchanged_file() {
        // `bad.tx` has a reference to an undeclared symbol, so the base holds index errors for it.
        let s = setup(&[
            ("bad.tx", "sym fn A\nlink A calls Nowhere\n"),
            ("ok.tx", "sym fn Ok\n"),
        ]);
        let p = RelPath::new("bad.tx").unwrap();
        {
            let (b, _) = engine(base_dir(&s.kx), &s.work);
            let r = b.cache().read().unwrap();
            assert!(!r.index_errors("r", &p).unwrap().is_empty());
        }
        let (e, n) = layer(&s);
        let rows = e.cache().read().unwrap().index_errors("r", &p).unwrap();
        assert!(!rows.is_empty(), "the copy carries the errors");
        rescan(&e);
        // Both files are unchanged from the seed, the failed one included: its revision is
        // stored beside its error rows, so nothing is extracted.
        assert_eq!(n.load(Ordering::SeqCst), 0, "nothing is extracted");
        assert_eq!(
            e.cache().read().unwrap().index_errors("r", &p).unwrap(),
            rows
        );
    }
}
