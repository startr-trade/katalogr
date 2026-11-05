mod context_store {
    use camino::Utf8PathBuf;
    use katalogr_store::{
        ContextId, ContextKind, ContextManifest, Kindex, ProjectConfig, ProjectName, RepoConfig,
        StoreError,
    };

    fn project(t: &tempfile::TempDir) -> katalogr_store::ProjectDir {
        let root = Utf8PathBuf::from_path_buf(t.path().join("kx")).unwrap();
        let kx = Kindex::init(&root).unwrap();
        let cfg = ProjectConfig {
            repos: vec![RepoConfig::new("r")],
            ..Default::default()
        };
        kx.create_project(&ProjectName::new("p").unwrap(), cfg)
            .unwrap()
    }

    fn manifest(id: ContextId) -> ContextManifest {
        ContextManifest {
            format: 1,
            id,
            kind: ContextKind::Checkout,
            repo: "r".into(),
            root: Some(Utf8PathBuf::from("/work/r")),
            seed_commit: Some("ab".repeat(20)),
        }
    }

    #[test]
    fn id_rejects_bad_text() {
        for bad in ["", "Base", "a_b", "a b", "a/b", "..", &"a".repeat(41)] {
            assert!(
                matches!(ContextId::new(bad), Err(StoreError::BadContext(_))),
                "{bad:?}"
            );
        }
        for ok in ["base", "w-0123456789abcdef", &"a".repeat(40)] {
            assert_eq!(ContextId::new(ok).unwrap().as_str(), ok);
        }
        let w = ContextId::for_checkout("/work/r".into());
        assert!(w.as_str().starts_with("w-") && w.as_str().len() == 18);
        let c = ContextId::for_commit("r", "abc");
        assert!(c.as_str().starts_with("c-") && c.as_str().len() == 18);
        assert_ne!(c, ContextId::for_commit("ra", "bc"));
        assert!(ContextId::new(w.as_str()).is_ok());
    }

    #[test]
    fn manifest_roundtrip_sorted() {
        let t = tempfile::tempdir().unwrap();
        let dir = Utf8PathBuf::from_path_buf(t.path().join("layer")).unwrap();
        let m = manifest(ContextId::for_checkout("/work/r".into()));
        m.write(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join("context.json")).unwrap();
        let keys: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("  \""))
            .map(|l| l.trim_start().split('"').nth(1).unwrap())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert_eq!(keys.len(), 6);
        assert_eq!(ContextManifest::read(&dir).unwrap(), m);
        assert_eq!(m.to_json().unwrap(), text);
        // No temp files left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn layer_paths_under_cache() {
        let t = tempfile::tempdir().unwrap();
        let p = project(&t);
        assert!(!p.is_layer());
        assert_eq!(p.project_root(), p.root());
        let id = ContextId::for_checkout("/work/r".into());
        let l = p.layer(&id);
        assert!(l.is_layer());
        assert_eq!(
            l.root(),
            p.root().join(".cache").join("contexts").join(id.as_str())
        );
        assert_eq!(l.project_root(), p.root());
        assert_eq!(l.cache_dir(), l.root().join(".cache"));
        assert_eq!(l.config().repos, p.config().repos);
        assert!(p.contexts().unwrap().is_empty());
        let a = ContextId::new("w-bbbb").unwrap();
        let b = ContextId::new("w-aaaa").unwrap();
        manifest(a.clone()).write(p.layer(&a).root()).unwrap();
        manifest(b.clone()).write(p.layer(&b).root()).unwrap();
        std::fs::create_dir_all(p.layer(&ContextId::new("junk").unwrap()).root()).unwrap();
        let ids: Vec<_> = l.contexts().unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec![b, a]);
    }

    #[test]
    fn layer_reads_project_profiles() {
        let t = tempfile::tempdir().unwrap();
        let p = project(&t);
        std::fs::write(p.project_root().join("profiles").join("x.yaml"), "a: 1\n").unwrap();
        let l = p.layer(&ContextId::new("w-aaaa").unwrap());
        assert!(l.project_root().join("profiles").join("x.yaml").is_file());
        assert!(!l.root().join("profiles").exists());
    }
}
