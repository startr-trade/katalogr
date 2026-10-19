mod kindex {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_model::*;
    use katalogr_store::marker::{self, Marker, Overrides};
    use katalogr_store::{
        Kindex, ProjectConfig, ProjectName, RepoConfig, SHARD_FORMAT, StoreError, shard,
    };
    use std::collections::BTreeMap;

    fn root(t: &tempfile::TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(t.path().join("kx")).unwrap()
    }

    fn pname(s: &str) -> ProjectName {
        ProjectName::new(s).unwrap()
    }

    fn cfg() -> ProjectConfig {
        ProjectConfig {
            repos: vec![RepoConfig::new("zeta"), RepoConfig::new("alpha")],
            ..Default::default()
        }
    }

    fn prov(path: &str) -> Provenance {
        Provenance {
            repo: "r".into(),
            path: RelPath::new(path).unwrap(),
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        }
    }

    fn sym(art: &ArtifactName, name: &str, facts: BTreeMap<String, Fact>) -> SymbolNode {
        SymbolNode {
            name: SymbolName {
                artifact: art.clone(),
                segments: vec![Segment {
                    kind: "item".into(),
                    name: name.into(),
                }],
                ordinal: 1,
            },
            kind: "item".into(),
            span: Span {
                start_line: 1,
                start_col: 1,
                end_line: 2,
                end_col: 1,
            },
            facts,
        }
    }

    fn fragment() -> Fragment {
        let art = ArtifactName::new("r", "src/a.txt").unwrap();
        let syms: Vec<SymbolNode> = ["b", "a", "c"]
            .iter()
            .map(|n| sym(&art, n, BTreeMap::new()))
            .collect();
        let ids: Vec<Id> = syms.iter().map(|s| s.name.id()).collect();
        let p = prov("src/a.txt");
        let links = vec![
            Link {
                from: ids[0],
                to: ids[1],
                kind: LinkKind("calls".into()),
                confidence: Confidence::Extracted,
                span: None,
                provenance: p.clone(),
            },
            Link {
                from: ids[2],
                to: ids[0],
                kind: LinkKind("calls".into()),
                confidence: Confidence::Inferred,
                span: None,
                provenance: p.clone(),
            },
            Link {
                from: ids[1],
                to: ids[2],
                kind: LinkKind("contains".into()),
                confidence: Confidence::Declared,
                span: None,
                provenance: p.clone(),
            },
        ];
        let key = |k: &str| ExportKey {
            namespace: "ns".into(),
            key: k.into(),
        };
        let refs = vec![
            OutRef {
                from: ids[2],
                kind: LinkKind("calls".into()),
                span: None,
                candidates: vec![key("y"), key("z")],
                confidence: Confidence::Extracted,
                origin: None,
            },
            OutRef {
                from: ids[0],
                kind: LinkKind("calls".into()),
                span: None,
                candidates: vec![key("x")],
                confidence: Confidence::Extracted,
                origin: None,
            },
        ];
        let exports = vec![
            Export {
                key: key("q"),
                target: ids[1],
            },
            Export {
                key: key("p"),
                target: ids[0],
            },
        ];
        Fragment {
            provenance: p,
            extension: "ext".into(),
            artifact: ArtifactNode {
                name: art,
                facts: BTreeMap::new(),
            },
            symbols: syms,
            links,
            refs,
            exports,
            bindings: Vec::new(),
        }
    }

    #[test]
    fn init_creates_project_layout() {
        let t = tempfile::tempdir().unwrap();
        let r = root(&t);
        let k = Kindex::init(&r).unwrap();
        assert!(r.join(".git").exists());
        assert_eq!(
            std::fs::read_to_string(r.join(".gitignore")).unwrap(),
            "projects/*/.cache/\n"
        );
        let p = k.create_project(&pname("demo"), cfg()).unwrap();
        for d in ["profiles", "shards", ".cache"] {
            assert!(p.root().join(d).is_dir(), "{d}");
        }
        assert!(p.root().join("format.json").is_file());
        let yaml = std::fs::read_to_string(p.root().join("config.yaml")).unwrap();
        assert!(yaml.find("alpha").unwrap() < yaml.find("zeta").unwrap());
        assert!(matches!(
            k.create_project(&pname("demo"), cfg()),
            Err(StoreError::ProjectExists(_))
        ));
        // idempotent
        Kindex::init(&r).unwrap();
        assert_eq!(
            std::fs::read_to_string(r.join(".gitignore")).unwrap(),
            "projects/*/.cache/\n"
        );
        let again = Kindex::open(&r).unwrap();
        assert_eq!(again.projects().unwrap(), vec![pname("demo")]);
        assert_eq!(
            again.project(&pname("demo")).unwrap().config().repos[0].name,
            "alpha"
        );
    }

    #[test]
    fn projects_isolated() {
        let t = tempfile::tempdir().unwrap();
        let k = Kindex::init(&root(&t)).unwrap();
        let a = k.create_project(&pname("one"), cfg()).unwrap();
        let b = k.create_project(&pname("two"), cfg()).unwrap();
        let f = fragment();
        a.write_shard(&f).unwrap();
        assert_eq!(a.list_shards().unwrap().len(), 1);
        assert!(b.list_shards().unwrap().is_empty());
        assert!(b.read_shard("r", &f.provenance.path).unwrap().is_none());
        assert_eq!(
            a.read_shard("r", &f.provenance.path)
                .unwrap()
                .unwrap()
                .extension,
            "ext"
        );
        assert_eq!(k.projects().unwrap(), vec![pname("one"), pname("two")]);
    }

    #[test]
    fn shards_deterministic() {
        let f = fragment();
        let bytes = shard::encode(&f).unwrap();
        let back = shard::decode(&bytes).unwrap();
        assert_eq!(shard::encode(&back).unwrap(), bytes);
        let mut g = f.clone();
        g.symbols.reverse();
        g.links.reverse();
        g.refs.reverse();
        g.exports.reverse();
        assert_eq!(shard::encode(&g).unwrap(), bytes);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.ends_with("}\n") && !s.ends_with("\n\n") && !s.contains('\r'));
        assert!(s.starts_with("{\n  \"artifact\""));
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["format"], 1);
        // symbols are sorted by canonical name
        let names: Vec<&str> = v["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert!(shard::decode(br#"{"format": 2}"#).is_err());
    }

    #[test]
    fn no_absolute_paths() {
        for bad in ["/home/x", "C:\\x", "~/x", "\\\\srv\\x"] {
            let mut f = fragment();
            let mut facts = BTreeMap::new();
            facts.insert("source".to_string(), Fact::Str(bad.to_string()));
            let art = f.artifact.name.clone();
            f.symbols[0] = sym(&art, "b", BTreeMap::new());
            f.symbols[1] = sym(&art, "a", BTreeMap::new());
            f.symbols[2] = sym(&art, "c", BTreeMap::new());
            f.symbols.push(sym(&art, "d", facts.clone()));
            f.symbols[3] = sym(&art, "d", facts);
            let e = shard::encode(&f).unwrap_err();
            let msg = e.to_string();
            assert!(msg.contains("symbols[3].facts.source"), "{msg}");
            assert!(!msg.contains(bad), "value must not be echoed: {msg}");
        }
        // Not machine-specific: routes, topics and XPath legitimately start with '/'.
        for fine in ["/api/users", "/orders/{id}", "/root-element/child"] {
            let mut f = fragment();
            let mut facts = BTreeMap::new();
            facts.insert("route".to_string(), Fact::Str(fine.to_string()));
            let art = f.artifact.name.clone();
            f.symbols[0] = sym(&art, "a", facts);
            assert!(shard::encode(&f).is_ok(), "{fine} must be allowed");
        }
    }

    #[test]
    fn provenance_recorded() {
        let f = fragment();
        let s = String::from_utf8(shard::encode(&f).unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["provenance"]["repo"], "r");
        assert_eq!(v["provenance"]["path"], "src/a.txt");
        assert_eq!(
            v["provenance"]["revision"],
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        let mut g = f;
        g.provenance.revision.clear();
        assert!(shard::encode(&g).is_err());
    }

    #[test]
    fn format_version_recorded() {
        let t = tempfile::tempdir().unwrap();
        let k = Kindex::init(&root(&t)).unwrap();
        let p = k.create_project(&pname("demo"), cfg()).unwrap();
        let s = std::fs::read_to_string(p.root().join("format.json")).unwrap();
        assert_eq!(
            s,
            "{\n  \"min_katalogr\": \"0.1.0\",\n  \"shard_format\": 1\n}\n"
        );
        assert_eq!(SHARD_FORMAT, 1);
    }

    #[test]
    fn newer_format_refused() {
        let t = tempfile::tempdir().unwrap();
        let k = Kindex::init(&root(&t)).unwrap();
        let p = k.create_project(&pname("demo"), cfg()).unwrap();
        std::fs::write(
            p.root().join("format.json"),
            "{\"min_katalogr\": \"9.9.0\", \"shard_format\": 2}\n",
        )
        .unwrap();
        let e = k.project(&pname("demo")).unwrap_err();
        assert!(
            matches!(e, StoreError::FormatTooNew { found: 2, .. }),
            "{e}"
        );
        assert!(e.to_string().contains("9.9.0"));
        std::fs::remove_file(p.root().join("format.json")).unwrap();
        assert!(matches!(
            k.project(&pname("demo")),
            Err(StoreError::NotAProject(_))
        ));
    }

    #[test]
    fn marker_resolves() {
        let t = tempfile::tempdir().unwrap();
        let repo = Utf8PathBuf::from_path_buf(t.path().join("repo")).unwrap();
        let nested = repo.join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        let m = Marker {
            kindex: Utf8PathBuf::from("../kx"),
            project: pname("demo"),
            repo: "alpha".into(),
        };
        marker::write(&repo, &m).unwrap();
        let no_env = |_: &str| None;
        let r = marker::resolve_with_env(&nested, &Overrides::default(), &no_env).unwrap();
        assert_eq!(r.kindex, repo.join("../kx"));
        assert_eq!(r.project, pname("demo"));
        assert_eq!(r.repo.as_deref(), Some("alpha"));
        assert_eq!(r.repo_root.as_deref(), Some(repo.as_path()));

        // env overrides marker; flags override env
        let env = |k: &str| match k {
            "KATALOGR_KINDEX" => Some("/env/kx".to_string()),
            "KATALOGR_PROJECT" => Some("envproj".to_string()),
            _ => None,
        };
        let r = marker::resolve_with_env(&nested, &Overrides::default(), &env).unwrap();
        assert_eq!(r.kindex, Utf8Path::new("/env/kx"));
        assert_eq!(r.project, pname("envproj"));
        let o = Overrides {
            kindex: Some("/flag/kx".into()),
            project: Some(pname("flagproj")),
        };
        let r = marker::resolve_with_env(&nested, &o, &env).unwrap();
        assert_eq!(r.kindex, Utf8Path::new("/flag/kx"));
        assert_eq!(r.project, pname("flagproj"));
        assert_eq!(r.repo.as_deref(), Some("alpha"));

        // nothing anywhere
        let bare = Utf8PathBuf::from_path_buf(t.path().join("bare")).unwrap();
        std::fs::create_dir_all(&bare).unwrap();
        assert!(matches!(
            marker::resolve_with_env(&bare, &Overrides::default(), &no_env),
            Err(StoreError::NotConfigured)
        ));
    }
}
