//! FEAT-007: the dependency inventory end to end, on synthetic temporary repositories.

mod deps {
    use katalogr_dist::analytics::{
        CommunityOptions, GraphOptions, HubOptions, Level, STRUCTURAL_KINDS,
    };
    use katalogr_dist::query::{DepHit, DepsOptions};
    use katalogr_dist::{Katalogr, Options, Outcome, Overrides, kindex_init, project_init};
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

    const CARGO_TOML: &str = "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nalpha-lib = \"1\"\n";
    const CARGO_LOCK: &str = "version = 3\n\n[[package]]\nname = \"alpha-lib\"\nversion = \"1.2.3\"\nsource = \"registry+https://registry.example/index\"\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"alpha-lib\",\n]\n";
    const LIB_RS: &str = "use alpha_lib::helper;\n\npub fn run() {\n    helper();\n}\n";
    const PACKAGE_JSON: &str = "{\"name\": \"web\", \"version\": \"1.0.0\", \"dependencies\": {\"beta-lib\": \"^2.0.0\"}}\n";
    const PACKAGE_LOCK: &str = "{\n  \"name\": \"web\",\n  \"version\": \"1.0.0\",\n  \"lockfileVersion\": 3,\n  \"packages\": {\n    \"\": {\n      \"name\": \"web\",\n      \"version\": \"1.0.0\",\n      \"dependencies\": {\n        \"beta-lib\": \"^2.0.0\"\n      }\n    },\n    \"node_modules/beta-lib\": {\n      \"version\": \"2.0.1\",\n      \"resolved\": \"https://registry.example/beta-lib/-/beta-lib-2.0.1.tgz\"\n    }\n  }\n}\n";
    const INDEX_TS: &str =
        "import { make } from 'beta-lib';\n\nexport function build() {\n  return make();\n}\n";

    struct World {
        _tmp: tempfile::TempDir,
        root: PathBuf,
    }

    /// A git repository `app` holding `files`, one project over it.
    fn world(files: &[(&str, &str)]) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let root = base.join("app");
        for (path, text) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        std::fs::write(root.join(".gitignore"), ".katalogr\n").unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "base"]);
        let kx = base.join("kx");
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".to_string(), root.clone())], true).unwrap();
        World {
            _tmp: tmp,
            root: root.clone(),
        }
    }

    fn open(w: &World) -> Katalogr {
        let k = Katalogr::resolve(&w.root, &Overrides::default()).unwrap();
        k.index(None, true, None, &Options::default()).unwrap();
        k
    }

    fn all(k: &Katalogr) -> Vec<DepHit> {
        let d = DepsOptions {
            package: None,
            limit: 1000,
            used_by_limit: 20,
        };
        match k.deps(&d, &Options::default()).unwrap().outcome {
            Outcome::Found(h) => h,
            other => panic!("{other:?}"),
        }
    }

    fn hit<'a>(hits: &'a [DepHit], package: &str) -> &'a DepHit {
        hits.iter()
            .find(|h| h.package == package)
            .unwrap_or_else(|| panic!("no {package} in {hits:#?}"))
    }

    fn full() -> World {
        world(&[
            ("Cargo.toml", CARGO_TOML),
            ("Cargo.lock", CARGO_LOCK),
            ("src/lib.rs", LIB_RS),
            ("web/package.json", PACKAGE_JSON),
            ("web/package-lock.json", PACKAGE_LOCK),
            ("web/src/index.ts", INDEX_TS),
        ])
    }

    #[test]
    fn rust_code_links_to_package() {
        let w = full();
        let k = open(&w);
        let hits = all(&k);
        let alpha = hit(&hits, "alpha-lib");
        assert_eq!(alpha.version, "1.2.3");
        assert_eq!(alpha.ecosystem, "cargo");
        assert_eq!(alpha.lockfile, "app:Cargo.lock");
        assert!(
            alpha.used_by.iter().any(|u| u.path == "src/lib.rs"),
            "{:#?}",
            alpha.used_by
        );
        assert_eq!(
            alpha.pinned_by,
            vec!["app:Cargo.lock#member:app".to_string()]
        );
    }

    #[test]
    fn ts_code_links_to_package() {
        let w = full();
        let k = open(&w);
        let hits = all(&k);
        let beta = hit(&hits, "beta-lib");
        assert_eq!(beta.ecosystem, "npm");
        assert!(
            beta.used_by.iter().any(|u| u.path == "web/src/index.ts"),
            "{:#?}",
            beta.used_by
        );
    }

    #[test]
    fn manifest_links_to_package() {
        let w = full();
        let k = open(&w);
        let hits = all(&k);
        let alpha = hit(&hits, "alpha-lib");
        assert!(
            alpha.used_by.iter().any(|u| u.path == "Cargo.toml"),
            "{:#?}",
            alpha.used_by
        );
    }

    #[test]
    fn two_lockfiles_fan_to_both() {
        let w = world(&[
            (
                "a/Cargo.toml",
                "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\nalpha-lib = \"1\"\n",
            ),
            ("a/Cargo.lock", CARGO_LOCK),
            ("a/src/lib.rs", LIB_RS),
            ("b/Cargo.lock", CARGO_LOCK),
        ]);
        let k = open(&w);
        let hits = all(&k);
        let mine: Vec<&DepHit> = hits.iter().filter(|h| h.package == "alpha-lib").collect();
        assert_eq!(mine.len(), 2, "{mine:#?}");
        // The code under `a` reaches the package in both lockfiles: an over-approximation.
        for h in mine {
            assert!(
                h.used_by.iter().any(|u| u.path == "a/src/lib.rs"),
                "{}: {:#?}",
                h.lockfile,
                h.used_by
            );
        }
    }

    #[test]
    fn version_path_reaches_code() {
        let w = full();
        let k = open(&w);
        let hits = all(&k);
        let alpha = hit(&hits, "alpha-lib");
        let user = alpha
            .used_by
            .iter()
            .find(|u| u.path == "src/lib.rs")
            .unwrap();
        let r = k
            .path(&alpha.name, &user.name, 6, &Options::default())
            .unwrap();
        match r.outcome {
            Outcome::Found(steps) => assert!(!steps.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn bad_lockfile_is_an_error_not_fatal() {
        let w = world(&[
            ("good/Cargo.lock", CARGO_LOCK),
            (
                "bad/Cargo.lock",
                "version = 3\n\n[[package]]\nname = \"zeta",
            ),
            ("old/package-lock.json", "{\"lockfileVersion\": 1}"),
            ("src/lib.rs", LIB_RS),
        ]);
        let k = open(&w);
        let hits = all(&k);
        assert!(
            hits.iter().all(|h| h.lockfile == "app:good/Cargo.lock"),
            "{hits:#?}"
        );
        assert_eq!(hits.len(), 1);
        // The other files were indexed.
        let f = k.find("run", &Options::default()).unwrap();
        assert!(matches!(f.outcome, Outcome::Found(_)));
    }

    #[test]
    fn lockfile_with_a_local_path_passes_the_guard() {
        let lock = PACKAGE_LOCK.replace(
            "https://registry.example/beta-lib/-/beta-lib-2.0.1.tgz",
            "file:/home/someone/vendor/beta-lib",
        );
        let w = world(&[("package-lock.json", &lock), ("package.json", PACKAGE_JSON)]);
        let k = open(&w);
        let hits = all(&k);
        let beta = hit(&hits, "beta-lib");
        assert_eq!(beta.source, None);
        // The lockfile itself was indexed.
        assert_eq!(beta.lockfile, "app:package-lock.json");
    }

    #[test]
    fn golden_lockfiles() {
        use katalogr_model::{ExtractInput, Extractor, Name, RelPath};
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/deps");
        for (case, file) in [
            ("cargo-lock", "Cargo.lock"),
            ("npm-lock", "package-lock.json"),
            ("pnpm-lock", "pnpm-lock.yaml"),
        ] {
            let text = std::fs::read(dir.join(case).join(file)).unwrap();
            let path = RelPath::new(file).unwrap();
            let input = ExtractInput {
                repo: "fx",
                path: &path,
                revision: "r",
                content: &text,
                context: &[],
            };
            let (a, errors) = katalogr_ext_deps::DepsExtractor
                .extract_with_errors(&input)
                .unwrap();
            let (b, _) = katalogr_ext_deps::DepsExtractor
                .extract_with_errors(&input)
                .unwrap();
            assert!(errors.is_empty(), "{case}: {errors:?}");
            assert_eq!(a, b, "{case}: not deterministic");
            // Symbols are the checked-in expectation, sorted.
            let want: serde_yaml_ng::Value = serde_yaml_ng::from_slice(
                &std::fs::read(dir.join(case).join("expected.yaml")).unwrap(),
            )
            .unwrap();
            let want: Vec<String> = want["symbols"]
                .as_sequence()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect();
            let have: Vec<String> = a.symbols.iter().map(|s| s.name.canonical()).collect();
            assert_eq!(have, want, "{case}");
            let mut sorted = have.clone();
            sorted.sort();
            assert_eq!(have, sorted);
        }
    }

    fn graph(level: Level) -> GraphOptions {
        let mut g = GraphOptions::new(level, true);
        g.exclude_kinds = STRUCTURAL_KINDS.iter().map(|k| k.to_string()).collect();
        g
    }

    #[test]
    fn map_lists_no_external_packages() {
        let w = full();
        let k = open(&w);
        let r = k.map(&Options::default()).unwrap();
        let Outcome::Found(entries) = r.outcome else {
            panic!()
        };
        assert!(!entries.is_empty());
        assert!(
            entries
                .iter()
                .all(|e| !e.symbol.kind.starts_with("package_external")
                    && e.symbol.kind != "package_version"),
            "{:?}",
            entries.iter().map(|e| &e.symbol.kind).collect::<Vec<_>>()
        );
        // The nodes exist: `find` still finds them.
        let f = k.find("alpha-lib", &Options::default()).unwrap();
        assert!(matches!(f.outcome, Outcome::Found(_)));
    }

    #[test]
    fn hubs_skip_external_packages() {
        let w = full();
        let k = open(&w);
        for level in [Level::Symbol, Level::File] {
            let o = HubOptions {
                graph: graph(level),
                min_degree: 0.0,
                limit: 1000,
            };
            let r = k.hubs(&o, &Options::default()).unwrap();
            let Outcome::Found(rep) = r.outcome else {
                panic!()
            };
            for item in &rep.items {
                assert!(
                    item.node.kind != "package_external" && item.node.kind != "package_version",
                    "{item:?}"
                );
            }
        }
    }

    #[test]
    fn structure_has_no_lockfile_edges() {
        let w = full();
        let k = open(&w);
        let o = CommunityOptions {
            graph: graph(Level::File),
            seed: 1,
            resolution: 1.0,
            min_size: 1,
            members: 100,
            limit: 100,
            of: None,
        };
        let r = k.communities(&o, &Options::default()).unwrap();
        let Outcome::Found(rep) = r.outcome else {
            panic!()
        };
        // No community joins a lockfile to the code that uses its packages.
        for c in &rep.items {
            let lock = c.top.iter().any(|n| {
                n.path
                    .as_deref()
                    .is_some_and(|p| p.ends_with("Cargo.lock") || p.ends_with("package-lock.json"))
            });
            let code = c.top.iter().any(|n| {
                n.path
                    .as_deref()
                    .is_some_and(|p| p.ends_with(".rs") || p.ends_with(".ts"))
            });
            assert!(!(lock && code), "{c:?}");
        }
    }
}
