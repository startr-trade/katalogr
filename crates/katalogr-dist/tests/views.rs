//! FEAT-032 T11: `view_graph` turns each view op into a graph through the query layer.
//! Needs the `.tx` test extractor: `cargo test -p katalogr-dist --features test-extractor`
//! (a workspace test run enables it through the CLI crate's dev-dependency).
#![cfg(feature = "test-extractor")]

mod views {
    use katalogr_dist::render::{ViewGraph, ViewSpec};
    use katalogr_dist::{Katalogr, Options, Outcome, Overrides, kindex_init, project_init};

    /// A project `demo` with one repo per `(repo, files)` entry; indexed.
    fn project(tmp: &std::path::Path, repos: &[(&str, &[(&str, &str)])]) -> Katalogr {
        let kx = tmp.join("kx");
        kindex_init(&kx).unwrap();
        let mut specs = Vec::new();
        for (name, files) in repos {
            let dir = tmp.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            for (f, text) in *files {
                std::fs::write(dir.join(f), text).unwrap();
            }
            specs.push((name.to_string(), dir));
        }
        let first = specs[0].1.clone();
        project_init(&kx, "demo", specs, true).unwrap();
        let k = Katalogr::resolve(
            &first,
            &Overrides {
                kindex: Some(kx.display().to_string()),
                project: None,
                context: None,
            },
        )
        .unwrap();
        k.index(None, true, None, &Options::default()).unwrap();
        k
    }

    fn view(k: &Katalogr, line: &str) -> Outcome<ViewGraph> {
        let spec = ViewSpec::parse(line).unwrap();
        k.view_graph(&spec, &Options::default()).unwrap().outcome
    }

    fn found(k: &Katalogr, line: &str) -> ViewGraph {
        match view(k, line) {
            Outcome::Found(g) => g,
            other => panic!("{line}: {other:?}"),
        }
    }

    fn names(g: &ViewGraph) -> Vec<&str> {
        g.nodes.iter().map(|n| n.name.as_str()).collect()
    }

    fn sorted(g: &ViewGraph) -> bool {
        g.nodes.windows(2).all(|w| w[0].id <= w[1].id)
            && g.edges
                .windows(2)
                .all(|w| (&w[0].from, &w[0].to, &w[0].kind) <= (&w[1].from, &w[1].to, &w[1].kind))
    }

    /// Three files: `f1` uses `f2`, `f2` uses `f3`.
    const CHAIN: &[(&str, &str)] = &[
        ("f1.tx", "sym fn a\nref a calls x:b\n"),
        ("f2.tx", "sym fn b\nexport x:b b\nref b calls x:c\n"),
        ("f3.tx", "sym fn c\nexport x:c c\n"),
    ];

    /// R-2: the structure view holds the ranked nodes and the edges between them, and says
    /// when the cap cut it.
    #[test]
    fn structure_from_graph() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        let g = found(&k, "structure --level file");
        assert_eq!(g.nodes.len(), 3, "{g:?}");
        assert_eq!((g.shown, g.total), (3, 3));
        assert_eq!(g.edges.len(), 2, "{g:?}");
        assert!(g.nodes.iter().all(|n| n.kind == "artifact"));
        assert!(sorted(&g));
        assert!(g.title.starts_with("structure --"), "{}", g.title);

        let capped = view(&k, "structure --level file --max-nodes 2");
        let Outcome::Found(c) = capped else {
            panic!("{capped:?}")
        };
        assert_eq!((c.shown, c.total), (2, 3));
        let r = k
            .view_graph(
                &ViewSpec::parse("structure --level file --max-nodes 2").unwrap(),
                &Options::default(),
            )
            .unwrap();
        assert_eq!(r.partial.len(), 1, "a cut view is marked partial");
        assert!(c.edges.len() <= 1, "only edges with both ends kept");
    }

    /// R-2: the contracts view joins each contract to its providers, consumers and declarers.
    #[test]
    fn contracts_lists_ends() {
        let tmp = tempfile::tempdir().unwrap();
        let files: &[(&str, &str)] = &[
            (
                "api.tx",
                "sym fn serve\nbind serve provides endpoint:orders\nsym fn spec\nbind spec declares endpoint:orders\n",
            ),
            (
                "client.tx",
                "sym fn fetch\nbind fetch consumes endpoint:orders\nsym fn other\nbind other provides queue:jobs\n",
            ),
        ];
        let k = project(tmp.path(), &[("app", files)]);
        let g = found(&k, "contracts");
        assert_eq!(g.nodes.iter().filter(|n| n.kind == "contract").count(), 2);
        let kinds: Vec<&str> = g.edges.iter().map(|e| e.kind.as_str()).collect();
        for role in ["provides", "consumes", "declares"] {
            assert!(kinds.contains(&role), "{role} in {kinds:?}");
        }
        assert!(sorted(&g));

        let only = found(&k, "contracts --kind queue");
        assert_eq!(
            only.nodes.iter().filter(|n| n.kind == "contract").count(),
            1
        );
        let cut = found(&k, "contracts --max-nodes 1");
        assert_eq!((cut.shown, cut.total), (1, 2));
    }

    /// R-2: the callers view is the reply's callers, each joined to what it calls.
    #[test]
    fn callers_from_reply() {
        let tmp = tempfile::tempdir().unwrap();
        let files: &[(&str, &str)] = &[(
            "a.tx",
            "sym fn target\nsym fn mid\nsym fn top\nsym fn lone\nlink mid calls target\nlink top calls mid\n",
        )];
        let k = project(tmp.path(), &[("app", files)]);
        let one = found(&k, "callers target --depth 1");
        assert_eq!(one.nodes.len(), 2, "{one:?}");
        assert_eq!(one.edges.len(), 1);
        let two = found(&k, "callers target --depth 2");
        assert_eq!(two.nodes.len(), 3, "{two:?}");
        assert_eq!(two.edges.len(), 2);
        assert!(two.edges.iter().all(|e| e.kind == "calls" && !e.inferred));
        assert!(sorted(&two));
        let cut = found(&k, "callers target --depth 2 --max-nodes 2");
        assert_eq!((cut.shown, cut.total), (2, 3));

        // Outcomes that are not answers pass through.
        assert!(matches!(
            view(&k, "callers nothing_like_this"),
            Outcome::NoMatch(_)
        ));
    }

    /// R-2: the path view is the reply's steps as nodes and edges.
    #[test]
    fn path_from_reply() {
        let tmp = tempfile::tempdir().unwrap();
        let files: &[(&str, &str)] = &[(
            "a.tx",
            "sym fn alpha\nsym fn beta\nsym fn gamma\nsym fn island\nlink alpha calls beta\nlink gamma calls beta\n",
        )];
        let k = project(tmp.path(), &[("app", files)]);
        let g = found(&k, "path alpha gamma");
        assert_eq!(g.nodes.len(), 3, "{g:?}");
        assert_eq!(g.edges.len(), 2);
        // Each edge follows the link, not the walk: both point at beta.
        let beta = g.nodes.iter().find(|n| n.name.ends_with("beta")).unwrap();
        assert!(g.edges.iter().all(|e| e.to == beta.id), "{g:?}");
        assert!(matches!(view(&k, "path alpha island"), Outcome::NoMatch(_)));
    }

    /// R-2: the cycles view draws each cycle's members and its witness.
    #[test]
    fn cycles_from_reply() {
        let tmp = tempfile::tempdir().unwrap();
        let files: &[(&str, &str)] = &[
            ("f1.tx", "sym fn a\nexport x:a a\nref a calls x:b\n"),
            ("f2.tx", "sym fn b\nexport x:b b\nref b calls x:a\n"),
            ("f3.tx", "sym fn c\nref c calls x:a\n"),
        ];
        let k = project(tmp.path(), &[("app", files)]);
        let g = found(&k, "cycles --level file");
        assert_eq!(g.nodes.len(), 2, "{g:?}");
        assert_eq!(g.edges.len(), 2, "{g:?}");
        assert!(sorted(&g));
        let tmp2 = tempfile::tempdir().unwrap();
        let none = found(
            &project(tmp2.path(), &[("app", CHAIN)]),
            "cycles --level file",
        );
        assert!(none.nodes.is_empty());
    }

    /// R-2: one project holding two repos gives one view, with nodes grouped by repo.
    #[test]
    fn spans_repos_in_one_project() {
        let tmp = tempfile::tempdir().unwrap();
        let api: &[(&str, &str)] = &[(
            "api.tx",
            "sym fn serve\nbind serve provides endpoint:orders\n",
        )];
        let web: &[(&str, &str)] = &[(
            "web.tx",
            "sym fn fetch\nbind fetch consumes endpoint:orders\n",
        )];
        let k = project(tmp.path(), &[("api", api), ("web", web)]);
        let g = found(&k, "contracts");
        let mut groups: Vec<&str> = g.nodes.iter().filter_map(|n| n.group.as_deref()).collect();
        groups.sort_unstable();
        assert_eq!(groups, ["api", "web"], "{g:?}");
        assert!(names(&g).iter().any(|n| n.starts_with("contract:")));
        let contract = g.nodes.iter().find(|n| n.kind == "contract").unwrap();
        assert!(contract.group.is_none());
        assert_eq!(g.edges.len(), 2);
    }
}
