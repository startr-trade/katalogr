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
                snapshot_dirs: Vec::new(),
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

    // ---- FEAT-088: the store, the hit path and the answer bytes ----

    use katalogr_dist::{Consistency, FreshCheck, ViewBytes};

    fn spec(line: &str) -> ViewSpec {
        ViewSpec::parse(line).unwrap()
    }

    /// The answer of `k.view` for `line`, found.
    fn answer(k: &Katalogr, line: &str, opts: &Options) -> (ViewBytes, katalogr_dist::Report) {
        let r = k.view(&spec(line), opts).unwrap();
        match r.outcome {
            Outcome::Found(b) => (b, r.report),
            other => panic!("{line}: {other:?}"),
        }
    }

    fn json(b: &ViewBytes) -> serde_json::Value {
        serde_json::from_slice(&b.body).unwrap()
    }

    /// Where the store keeps `id` for this project.
    fn entry(k: &Katalogr, id: &str) -> std::path::PathBuf {
        k.project()
            .cache_dir()
            .as_std_path()
            .join("views")
            .join(&id[3..5])
            .join(format!("{id}.json"))
    }

    fn stale_ok() -> Options {
        Options {
            consistency: Some(Consistency::StaleOk),
            ..Options::default()
        }
    }

    const LINE: &str = "structure --level file";

    /// R-2: the second read of a question comes from the store, byte for byte.
    #[test]
    fn second_read_served_from_store() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        let (first, report) = answer(&k, LINE, &Options::default());
        assert!(!first.from_store);
        assert!(report.exact, "{report:?}");
        let id = report.view_id.expect("an exact view has an id");
        assert!(entry(&k, &id).is_file());
        let (second, again) = answer(&k, LINE, &Options::default());
        assert!(second.from_store);
        assert_eq!(second.body, first.body);
        assert_eq!(again.view_id.as_deref(), Some(id.as_str()));
        assert!(again.exact);
    }

    /// R-2: a hit runs no query: an entry changed on disk is what comes back.
    #[test]
    fn hit_skips_query() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        let (first, report) = answer(&k, LINE, &Options::default());
        let id = report.view_id.unwrap();
        let mut doctored = json(&first);
        doctored["mermaid"] = "STORED".into();
        std::fs::write(entry(&k, &id), serde_json::to_vec(&doctored).unwrap()).unwrap();
        let (second, _) = answer(&k, LINE, &Options::default());
        assert!(second.from_store);
        assert_eq!(json(&second)["mermaid"], "STORED");
    }

    /// R-2: a hit does not wait for the queue and does not drain it.
    #[test]
    fn hit_needs_no_drain() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        answer(&k, LINE, &Options::default());
        // A change event for a file that did not change: the position stays.
        k.notify(&["f1.tx".to_string()], false).unwrap();
        let before = k.status(&stale_ok()).unwrap().pending;
        assert!(before > 0, "the event is queued");
        let (hit, _) = answer(&k, LINE, &Options::default());
        assert!(hit.from_store, "the position did not move");
        assert_eq!(k.status(&stale_ok()).unwrap().pending, before);
    }

    /// R-2: a stored view answers a position no context holds any more.
    #[test]
    fn hit_at_position_no_context_holds() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        let (_, report) = answer(&k, LINE, &Options::default());
        let held = report.positions[0].text();
        std::fs::write(tmp.path().join("app/f4.tx"), "sym fn d\n").unwrap();
        k.index(None, true, None, &Options::default()).unwrap();
        let at = Options {
            at: Some(held.clone()),
            ..Options::default()
        };
        // Nothing holds that position now ...
        let gone = k.view_graph(&spec(LINE), &at).unwrap();
        assert!(
            matches!(gone.outcome, Outcome::PositionUnavailable { .. }),
            "{:?}",
            gone.outcome
        );
        // ... and the store still answers it.
        let (hit, r) = answer(&k, LINE, &at);
        assert!(hit.from_store);
        assert_eq!(r.positions[0].text(), held);
    }

    /// R-2: the ID known before the read is the ID of the answer, for one repo and for two.
    #[test]
    fn preflight_id_equals_answer_id() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        let (id, positions) = k
            .view_preflight(&spec(LINE), &Options::default())
            .unwrap()
            .expect("a position can be read");
        assert_eq!(positions.len(), 1);
        let (_, report) = answer(&k, LINE, &Options::default());
        assert_eq!(report.view_id.as_deref(), Some(id.as_str()));
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        let tmp2 = tempfile::tempdir().unwrap();
        let api: &[(&str, &str)] = &[("api.tx", "sym fn serve\n")];
        let web: &[(&str, &str)] = &[("web.tx", "sym fn fetch\n")];
        let k2 = project(tmp2.path(), &[("api", api), ("web", web)]);
        let (id2, positions2) = k2
            .view_preflight(&spec("contracts"), &Options::default())
            .unwrap()
            .unwrap();
        assert_eq!(positions2.len(), 2);
        let (_, report2) = answer(&k2, "contracts", &Options::default());
        assert_eq!(report2.view_id.as_deref(), Some(id2.as_str()));
    }

    /// R-3: an exact body has positions, no watermark and no absolute path.
    #[test]
    fn stored_body_has_no_watermark() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        let (_, report) = answer(&k, LINE, &Options::default());
        let id = report.view_id.unwrap();
        let text = std::fs::read_to_string(entry(&k, &id)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        for absent in ["watermark", "pending", "stale"] {
            assert!(v.get(absent).is_none(), "{absent} in {text}");
        }
        assert_eq!(v["exact"], true);
        assert_eq!(v["view_id"], id.as_str());
        assert!(v["positions"].as_array().is_some_and(|p| !p.is_empty()));
        let root = tmp.path().to_str().unwrap();
        assert!(!text.contains(root), "the checkout's root is in {text}");
    }

    /// R-3: the same question at the same position gives the same bytes, in any project.
    #[test]
    fn same_id_same_bytes() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let ka = project(a.path(), &[("app", CHAIN)]);
        let kb = project(b.path(), &[("app", CHAIN)]);
        let (x, rx) = answer(&ka, LINE, &Options::default());
        let (y, ry) = answer(&kb, LINE, &Options::default());
        assert_eq!(rx.view_id, ry.view_id);
        assert_eq!(x.body, y.body);
    }

    /// R-3: two spellings of one question share an ID.
    #[test]
    fn defaults_normalise_into_id() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        let (_, a) = answer(&k, "structure", &Options::default());
        let (_, b) = answer(
            &k,
            "structure --max-nodes 60 --level package --inferred false",
            &Options::default(),
        );
        assert!(a.view_id.is_some());
        assert_eq!(a.view_id, b.view_id);
        let (_, c) = answer(&k, "structure --level file", &Options::default());
        assert_ne!(a.view_id, c.view_id);
    }

    /// R-4: an answer that is not exact is returned, marked, and never stored.
    #[test]
    fn not_exact_not_stored() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        let (body, report) = answer(&k, LINE, &stale_ok());
        assert!(!report.exact);
        assert!(!body.from_store);
        let v = json(&body);
        assert_eq!(v["exact"], false);
        assert!(v.get("view_id").is_none());
        for present in ["watermark", "pending", "stale"] {
            assert!(v.get(present).is_some(), "{present} in {v}");
        }
        let views = k.project().cache_dir().as_std_path().join("views");
        let stored = walk(&views);
        assert!(stored.is_empty(), "{stored:?}");
    }

    fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    out.extend(walk(&e.path()));
                } else {
                    out.push(e.path());
                }
            }
        }
        out
    }

    /// R-5: a new position is a new ID.
    #[test]
    fn new_position_new_id() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        let (a, ra) = answer(&k, LINE, &Options::default());
        std::fs::write(tmp.path().join("app/f4.tx"), "sym fn d\n").unwrap();
        let (b, rb) = answer(&k, LINE, &Options::default());
        assert!(!b.from_store);
        assert_ne!(ra.view_id, rb.view_id);
        assert_ne!(json(&a)["positions"], json(&b)["positions"]);
    }

    /// R-5: a profile change, once the index has taken it, makes the next read a miss.
    #[test]
    fn profile_change_misses() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        let (_, ra) = answer(&k, LINE, &Options::default());
        let dir = k.project().root().as_std_path().join("profiles");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("example.yaml"),
            "profile: example\nversion: 1\nrules:\n  - id: r\n    when: { extension: test, kind: fn, name: '^zzz$' }\n    emit:\n      bind: { contract: { kind: channel, key: c }, role: provides }\n",
        )
        .unwrap();
        k.index(None, true, None, &Options::default()).unwrap();
        let (b, rb) = answer(&k, LINE, &Options::default());
        assert!(!b.from_store);
        assert_ne!(ra.view_id, rb.view_id);
    }

    /// R-10: after the store loses an entry, the same bytes are made again.
    #[test]
    fn pruned_view_recomputes_same_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        let (first, report) = answer(&k, LINE, &Options::default());
        let id = report.view_id.unwrap();
        std::fs::remove_file(entry(&k, &id)).unwrap();
        let (second, _) = answer(&k, LINE, &Options::default());
        assert!(!second.from_store);
        assert_eq!(second.body, first.body);
    }

    /// R-11: a fresh computation equals the stored entry; a changed entry is a difference.
    #[test]
    fn fresh_check_compares() {
        let tmp = tempfile::tempdir().unwrap();
        let k = project(tmp.path(), &[("app", CHAIN)]);
        assert_eq!(
            k.view_fresh(&spec(LINE), &Options::default()).unwrap(),
            FreshCheck::NotStored
        );
        let (first, report) = answer(&k, LINE, &Options::default());
        let id = report.view_id.unwrap();
        assert_eq!(
            k.view_fresh(&spec(LINE), &Options::default()).unwrap(),
            FreshCheck::Same
        );
        let mut doctored = json(&first);
        doctored["mermaid"] = "OLD".into();
        std::fs::write(entry(&k, &id), serde_json::to_vec(&doctored).unwrap()).unwrap();
        assert_eq!(
            k.view_fresh(&spec(LINE), &Options::default()).unwrap(),
            FreshCheck::Different { view_id: id }
        );
    }
}
