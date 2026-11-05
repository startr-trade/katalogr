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
                binds: None,
            },
            OutRef {
                from: ids[0],
                kind: LinkKind("calls".into()),
                span: None,
                candidates: vec![key("x")],
                confidence: Confidence::Extracted,
                origin: None,
                binds: None,
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

    fn git(root: &Utf8Path, args: &[&str]) {
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
            .current_dir(root)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn frag(path: &str) -> Fragment {
        let art = ArtifactName::new("r", path).unwrap();
        Fragment {
            provenance: prov(path),
            extension: "ext".into(),
            artifact: ArtifactNode {
                name: art,
                facts: BTreeMap::new(),
            },
            symbols: vec![],
            links: vec![],
            refs: vec![],
            exports: vec![],
            bindings: vec![],
        }
    }

    fn key(path: &str) -> (String, RelPath) {
        ("r".to_string(), RelPath::new(path).unwrap())
    }

    #[test]
    fn git_state_variants() {
        use katalogr_store::GitState;
        let t = tempfile::tempdir().unwrap();
        let r = root(&t);
        let kx = Kindex::init(&r).unwrap();
        let dir = kx
            .create_project(
                &pname("p"),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        for p in ["a.txt", "b.txt", "c.txt"] {
            dir.write_shard(&frag(p)).unwrap();
        }

        // A repository with no commit.
        assert_eq!(
            dir.git_state().unwrap(),
            GitState::NoCommits { shard_files: 3 }
        );

        // Clean.
        git(&r, &["add", "-A"]);
        git(&r, &["commit", "-m", "index"]);
        match dir.git_state().unwrap() {
            GitState::Repo {
                head,
                modified,
                added,
                deleted,
            } => {
                assert_eq!(head.len(), 40);
                assert!(modified.is_empty() && added.is_empty() && deleted.is_empty());
            }
            other => panic!("{other:?}"),
        }

        // Dirty: one edited, one added, one removed.
        let mut edited = frag("a.txt");
        edited.extension = "other".into();
        dir.write_shard(&edited).unwrap();
        dir.write_shard(&frag("d.txt")).unwrap();
        std::fs::remove_file(dir.shard_path("r", &RelPath::new("c.txt").unwrap())).unwrap();
        match dir.git_state().unwrap() {
            GitState::Repo {
                modified,
                added,
                deleted,
                ..
            } => {
                assert_eq!(modified, vec![key("a.txt")]);
                assert_eq!(added, vec![key("d.txt")]);
                assert_eq!(deleted, vec![key("c.txt")]);
            }
            other => panic!("{other:?}"),
        }

        // Not a repository.
        std::fs::remove_dir_all(r.join(".git")).unwrap();
        assert_eq!(dir.git_state().unwrap(), GitState::NotARepo);
    }

    #[test]
    fn format_file_reads_without_version_check() {
        let t = tempfile::tempdir().unwrap();
        let r = root(&t);
        let kx = Kindex::init(&r).unwrap();
        kx.create_project(&pname("p"), ProjectConfig::default())
            .unwrap();
        assert_eq!(
            kx.format_file(&pname("p")).unwrap().shard_format,
            SHARD_FORMAT
        );
        std::fs::write(
            r.join("projects/p/format.json"),
            r#"{"min_katalogr":"9.9.9","shard_format":99}"#,
        )
        .unwrap();
        assert!(matches!(
            kx.project(&pname("p")),
            Err(StoreError::FormatTooNew { .. })
        ));
        let f = kx.format_file(&pname("p")).unwrap();
        assert_eq!((f.shard_format, f.min_katalogr.as_str()), (99, "9.9.9"));
        std::fs::write(r.join("projects/p/format.json"), "{").unwrap();
        assert!(matches!(
            kx.format_file(&pname("p")),
            Err(StoreError::Parse { .. })
        ));
    }

    use katalogr_store::{needles_from, scan_for_absolute_with};
    use std::time::Instant;

    fn gfrag(sym_name: &str, facts: BTreeMap<String, Fact>) -> Fragment {
        let art = ArtifactName::new("r", "src/a.ts").unwrap();
        Fragment {
            provenance: Provenance {
                repo: "r".into(),
                path: RelPath::new("src/a.ts").unwrap(),
                revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
            },
            extension: "test".into(),
            artifact: ArtifactNode {
                name: art.clone(),
                facts: BTreeMap::new(),
            },
            symbols: vec![SymbolNode {
                name: SymbolName {
                    artifact: art,
                    segments: vec![Segment {
                        kind: "fn".into(),
                        name: sym_name.into(),
                    }],
                    ordinal: 1,
                },
                kind: "fn".into(),
                span: Span {
                    start_line: 1,
                    start_col: 1,
                    end_line: 1,
                    end_col: 2,
                },
                facts,
            }],
            links: vec![],
            refs: vec![],
            exports: vec![],
            bindings: vec![],
        }
    }

    fn with_fact(text: &str) -> Fragment {
        let mut m = BTreeMap::new();
        m.insert("source".to_string(), Fact::Str(text.to_string()));
        gfrag("f", m)
    }

    #[test]
    fn relative_needle_ignored() {
        assert!(needles_from(["tmp", ".", "/", "", "C:\\"]).is_empty());
        let needles = needles_from(["tmp"]);
        let mut m = BTreeMap::new();
        m.insert(
            "str_calls".to_string(),
            Fact::Str("path.join(portableDataPath, \"tmp\")".into()),
        );
        assert_eq!(
            scan_for_absolute_with(&gfrag("tmpDir", m), &needles),
            Ok(())
        );
    }

    #[test]
    fn needle_matches_whole_path_only() {
        let n = needles_from(["/home/synthuser", "/tmp"]);
        assert_eq!(n, vec!["/home/synthuser".to_string(), "/tmp".to_string()]);
        for fine in [
            "tmpDir",
            "/api/tmpl",
            "see /home/synthusereloper/x",
            "x/home/synthuser",
            "/tmp-x",
        ] {
            assert!(
                scan_for_absolute_with(&with_fact(fine), &n).is_ok(),
                "{fine} must pass"
            );
        }
        for bad in [
            "cat /home/synthuser/x",
            "file:///home/synthuser",
            "run /home/synthuser",
            "a /tmp",
        ] {
            assert!(
                scan_for_absolute_with(&with_fact(bad), &n).is_err(),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn machine_paths_still_refused() {
        let n = needles_from(["/home/synthuser", "/opt/scratch", "/srv/kx"]);
        for bad in [
            "open /home/synthuser/data.txt",
            "in /opt/scratch/x",
            "root /srv/kx",
            "C:\\x",
            "d:/x",
            "\\\\srv\\share",
            "file:///home/synthuser/x",
            "~/x",
            "/home/other",
        ] {
            let e = scan_for_absolute_with(&with_fact(bad), &n).unwrap_err();
            let msg = e.to_string();
            assert!(msg.contains("symbols[0].facts.source"), "{msg}");
            assert!(!msg.contains(bad), "value echoed: {msg}");
            assert!(!msg.contains("synthuser"), "value echoed: {msg}");
        }
    }

    #[test]
    #[ignore = "release build only"]
    fn guard_scan_budget() {
        let mut f = gfrag("f", BTreeMap::new());
        let proto = f.symbols[0].clone();
        f.symbols.clear();
        for i in 0..5000 {
            let mut s = proto.clone();
            s.name.segments[0].name = format!("sym{i}");
            for k in 0..8 {
                s.facts.insert(
                    format!("fact{k}"),
                    Fact::Str(format!("path.join(base, \"segment{i}\", \"item{k}\")")),
                );
            }
            f.symbols.push(s);
        }
        let n = needles_from(["/home/synthuser", "/opt/scratch", "/srv/kx"]);
        let t = Instant::now();
        scan_for_absolute_with(&f, &n).unwrap();
        let ms = t.elapsed().as_millis();
        println!("guard_scan_budget: {ms} ms");
        assert!(ms < 25, "{ms} ms");
    }
}
