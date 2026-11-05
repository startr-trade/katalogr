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
        assert!(e.dir().list_shards().unwrap().is_empty());
        // One edit: one extraction and one delta shard, and a rescan then does nothing more.
        std::fs::write(s.work.join("b.tx"), "sym fn B\nsym fn B2\n").unwrap();
        let g = changed(&e, "b.tx", EventKind::Changed);
        assert!(g.contains("B2"));
        assert_eq!(n.load(Ordering::SeqCst), 1);
        assert_eq!(rescan(&e), g);
        assert_eq!(n.load(Ordering::SeqCst), 1);
        assert_eq!(e.dir().list_shards().unwrap().len(), 1);
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

    #[test]
    fn base_shard_with_errors_not_reused() {
        // `bad.tx` has a reference to an undeclared symbol, so the base holds index errors for it.
        let bad = "sym fn A\nlink A calls Nowhere\n";
        let s = setup(&[("bad.tx", bad), ("ok.tx", "sym fn Ok\n")]);
        {
            let (b, _) = engine(base_dir(&s.kx), &s.work);
            let r = b.cache().read().unwrap();
            let p = RelPath::new("bad.tx").unwrap();
            assert!(!r.index_errors("r", &p).unwrap().is_empty());
        }
        let (e, n) = layer(&s);
        // Edit both away from the committed text and back: the sound file is reused from the
        // base's shard without extracting, the one with errors is extracted again.
        for (f, other) in [("bad.tx", "sym fn A\n"), ("ok.tx", "sym fn Ok\nsym fn X\n")] {
            let committed = std::fs::read_to_string(s.work.join(f)).unwrap();
            std::fs::write(s.work.join(f), other).unwrap();
            changed(&e, f, EventKind::Changed);
            let after_edit = n.load(Ordering::SeqCst);
            std::fs::write(s.work.join(f), committed).unwrap();
            changed(&e, f, EventKind::Changed);
            let revert_cost = n.load(Ordering::SeqCst) - after_edit;
            if f == "bad.tx" {
                assert_eq!(revert_cost, 1, "errored base shard must be extracted");
                let r = e.cache().read().unwrap();
                let p = RelPath::new(f).unwrap();
                assert!(!r.index_errors("r", &p).unwrap().is_empty());
            } else {
                assert_eq!(revert_cost, 0, "sound base shard is reused");
            }
        }
    }

    #[test]
    fn branch_switch_reuses_base_shard() {
        let s = setup(&[("a.tx", "sym fn A\n"), ("b.tx", "sym fn B\n")]);
        let (e, n) = layer(&s);
        let clean = drained_graph(&e);
        // "Switch" to a branch where a.tx differs, then back.
        std::fs::write(s.work.join("a.tx"), "sym fn A\nsym fn Side\n").unwrap();
        changed(&e, "a.tx", EventKind::Changed);
        assert_eq!(n.load(Ordering::SeqCst), 1);
        std::fs::write(s.work.join("a.tx"), "sym fn A\n").unwrap();
        let back = changed(&e, "a.tx", EventKind::Changed);
        assert_eq!(n.load(Ordering::SeqCst), 1, "the base's shard was reused");
        assert_eq!(back, clean);
        // The reused shard is the layer's own copy, byte-identical to the base's.
        let p = RelPath::new("a.tx").unwrap();
        let mine = std::fs::read(e.dir().shard_path("r", &p)).unwrap();
        let theirs = std::fs::read(base_dir(&s.kx).shard_path("r", &p)).unwrap();
        assert_eq!(mine, theirs);
    }
}
