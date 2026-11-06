mod support;

mod impact {
    use super::support::*;
    use katalogr_model::{Confidence, Id};
    use katalogr_query::result::cap;
    use katalogr_query::{
        ImpactOptions, ImpactReport, SEED_CONTRACT, SEED_CONTRACT_CONSUMERS, SEED_SYMBOL, Seed,
        impact,
    };
    use katalogr_store::{Cache, WriteTx};

    fn seed(id: Id) -> Seed {
        Seed {
            id,
            precision: SEED_SYMBOL,
            wanted: None,
        }
    }

    fn build(repos: &[&str], f: impl FnOnce(&mut WriteTx)) -> (tempfile::TempDir, Cache) {
        let tmp = tempfile::tempdir().unwrap();
        let cache = open(&tmp, repos);
        {
            let mut tx = cache.write().unwrap();
            f(&mut tx);
            tx.commit().unwrap();
        }
        (tmp, cache)
    }

    fn run(cache: &Cache, seeds: &[Seed], opts: &ImpactOptions) -> ImpactReport {
        let tx = cache.read().unwrap();
        impact(&tx, seeds, opts).unwrap()
    }

    fn names(r: &ImpactReport, kind: &str) -> Vec<String> {
        r.groups
            .iter()
            .filter(|g| g.kind == kind)
            .flat_map(|g| g.hits.iter().map(|h| h.node.name.clone()))
            .collect()
    }

    fn all_hits(r: &ImpactReport) -> Vec<String> {
        let mut v: Vec<String> = r
            .groups
            .iter()
            .flat_map(|g| g.hits.iter().map(|h| h.node.name.clone()))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn symbol_reaches_callers_and_refs() {
        let (_t, c) = build(&["r"], |tx| {
            let (_, a) = artifact(tx, "r", "a.tx", &[("load", 1, 2), ("serve", 3, 4)]);
            let (_, b) = artifact(tx, "r", "b.tx", &[("main", 1, 2), ("note", 3, 4)]);
            link(tx, "r", "a.tx", a[1], "calls", a[0]);
            link(tx, "r", "b.tx", b[0], "calls", a[1]);
            link(tx, "r", "b.tx", b[1], "references", a[0]);
        });
        let seed_id = Id::of("r:a.tx#fn:load");
        let r = run(&c, &[seed(seed_id)], &ImpactOptions::default());
        assert_eq!(r.seeds.len(), 1);
        assert_eq!(r.seeds[0].precision, "symbol");
        assert_eq!(names(&r, "calls"), ["r:a.tx#fn:serve", "r:b.tx#fn:main"]);
        assert_eq!(names(&r, "references"), ["r:b.tx#fn:note"]);
        let main = &r.groups.iter().find(|g| g.kind == "calls").unwrap().hits[1];
        assert_eq!(main.depth, 2);
        assert_eq!(main.chain, ["r:a.tx#fn:serve"]);
        assert_eq!(main.seed, "r:a.tx#fn:load");
        assert_eq!(r.total, Some(3));
        assert!(r.partial.is_empty());
        assert_eq!(r.repos, ["r"]);
        assert!(r.projects.is_empty());
    }

    #[test]
    fn each_node_once_with_best_path() {
        let (_t, c) = build(&["r"], |tx| {
            let (_, a) = artifact(tx, "r", "a.tx", &[("load", 1, 2), ("x", 3, 4), ("y", 5, 6)]);
            // x reaches load at the same depth by an inferred link and an extracted one.
            link_conf(
                tx,
                "r",
                "a.tx",
                a[1],
                "references",
                a[0],
                Confidence::Inferred,
            );
            link(tx, "r", "a.tx", a[1], "calls", a[0]);
            link(tx, "r", "a.tx", a[2], "calls", a[1]);
        });
        let r = run(
            &c,
            &[seed(Id::of("r:a.tx#fn:load"))],
            &ImpactOptions::default(),
        );
        let xs: Vec<_> = r
            .groups
            .iter()
            .flat_map(|g| g.hits.iter().map(move |h| (g, h)))
            .filter(|(_, h)| h.node.name == "r:a.tx#fn:x")
            .collect();
        assert_eq!(xs.len(), 1);
        let (g, h) = xs[0];
        assert_eq!(
            (g.kind.as_str(), g.confidence),
            ("calls", Confidence::Extracted)
        );
        assert_eq!(h.depth, 1);
        assert!(h.chain.is_empty());
        assert_eq!(all_hits(&r), ["r:a.tx#fn:x", "r:a.tx#fn:y"]);
    }

    #[test]
    fn walks_through_reexports() {
        let (_t, c) = build(&["r"], |tx| {
            let (_, a) = artifact(
                tx,
                "r",
                "a.tx",
                &[("load", 1, 2), ("reload", 3, 4), ("use_it", 5, 6)],
            );
            link(tx, "r", "a.tx", a[1], "reexports", a[0]);
            link(tx, "r", "a.tx", a[2], "calls", a[1]);
        });
        let r = run(
            &c,
            &[seed(Id::of("r:a.tx#fn:load"))],
            &ImpactOptions::default(),
        );
        assert_eq!(names(&r, "reexports"), ["r:a.tx#fn:reload"]);
        assert_eq!(names(&r, "calls"), ["r:a.tx#fn:use_it"]);
        let h = &r.groups.iter().find(|g| g.kind == "calls").unwrap().hits[0];
        assert_eq!(h.chain, ["r:a.tx#fn:reload"]);
    }

    /// `svc` provides a contract that `app` consumes.
    fn two_repos() -> (tempfile::TempDir, Cache) {
        build(&["app", "svc"], |tx| {
            let (_, s) = artifact(tx, "svc", "s.tx", &[("handler", 1, 2), ("helper", 3, 4)]);
            let (_, a) = artifact(tx, "app", "a.tx", &[("client", 1, 2), ("page", 3, 4)]);
            let c = contract(tx, "channel", "orders", 1, 1, 0);
            link(tx, "svc", "s.tx", s[0], "provides", c);
            link(tx, "app", "a.tx", a[0], "consumes", c);
            link(tx, "app", "a.tx", a[1], "calls", a[0]);
            link(tx, "svc", "s.tx", s[0], "calls", s[1]);
        })
    }

    #[test]
    fn crosses_contract_and_repo() {
        let (_t, c) = two_repos();
        let r = run(
            &c,
            &[seed(Id::of("svc:s.tx#fn:helper"))],
            &ImpactOptions::default(),
        );
        // helper <- handler (calls) <- client (consumes, through the contract) <- page (calls)
        assert_eq!(
            names(&r, "calls"),
            ["svc:s.tx#fn:handler", "app:a.tx#fn:page"]
        );
        assert_eq!(names(&r, "consumes"), ["app:a.tx#fn:client"]);
        let client = &r.groups.iter().find(|g| g.kind == "consumes").unwrap().hits[0];
        assert_eq!(client.via.as_deref(), Some("contract:channel:orders"));
        assert_eq!(client.depth, 2);
        assert_eq!(client.chain, ["svc:s.tx#fn:handler"]);
        assert_eq!(r.repos, ["app", "svc"]);
        assert_eq!(r.contracts.len(), 1);
        assert_eq!(r.contracts[0].contract, "contract:channel:orders");
        assert_eq!(r.contracts[0].open, None);
    }

    fn both_sides() -> (tempfile::TempDir, Cache) {
        build(&["r"], |tx| {
            let (_, f) = artifact(
                tx,
                "r",
                "f.tx",
                &[("p1", 1, 2), ("p2", 3, 4), ("c1", 5, 6), ("d1", 7, 8)],
            );
            let c = contract(tx, "channel", "orders", 2, 1, 1);
            link(tx, "r", "f.tx", f[0], "provides", c);
            link(tx, "r", "f.tx", f[1], "provides", c);
            link(tx, "r", "f.tx", f[2], "consumes", c);
            link(tx, "r", "f.tx", f[3], "declares", c);
        })
    }

    #[test]
    fn contract_seed_reports_both_sides() {
        let (_t, c) = both_sides();
        let s = Seed {
            id: Id::of("contract:channel:orders"),
            precision: SEED_CONTRACT,
            wanted: None,
        };
        let r = run(&c, &[s], &ImpactOptions::default());
        assert_eq!(names(&r, "provides"), ["r:f.tx#fn:p1", "r:f.tx#fn:p2"]);
        assert_eq!(names(&r, "consumes"), ["r:f.tx#fn:c1"]);
        assert_eq!(names(&r, "declares"), ["r:f.tx#fn:d1"]);
        assert_eq!(r.seeds[0].node.kind, "contract");
        assert_eq!(r.contracts.len(), 1);
    }

    #[test]
    fn declares_reaches_both_sides() {
        let (_t, c) = both_sides();
        let r = run(
            &c,
            &[seed(Id::of("r:f.tx#fn:d1"))],
            &ImpactOptions::default(),
        );
        assert_eq!(names(&r, "provides"), ["r:f.tx#fn:p1", "r:f.tx#fn:p2"]);
        assert_eq!(names(&r, "consumes"), ["r:f.tx#fn:c1"]);
        assert_eq!(r.contracts.len(), 1);
    }

    #[test]
    fn consumer_change_stops_at_contract() {
        let (_t, c) = both_sides();
        let r = run(
            &c,
            &[seed(Id::of("r:f.tx#fn:c1"))],
            &ImpactOptions::default(),
        );
        assert!(r.groups.is_empty());
        assert!(r.contracts.is_empty());
    }

    #[test]
    fn contract_consumers_seed_skips_providers() {
        let (_t, c) = both_sides();
        let s = Seed {
            id: Id::of("contract:channel:orders"),
            precision: SEED_CONTRACT_CONSUMERS,
            wanted: None,
        };
        let r = run(&c, &[s], &ImpactOptions::default());
        assert_eq!(all_hits(&r), ["r:f.tx#fn:c1"]);
        assert_eq!(r.groups[0].kind, "consumes");
    }

    #[test]
    fn groups_by_kind_and_confidence() {
        let (_t, c) = build(&["r"], |tx| {
            let (_, a) = artifact(
                tx,
                "r",
                "a.tx",
                &[
                    ("t", 1, 2),
                    ("a", 3, 4),
                    ("b", 5, 6),
                    ("c", 7, 8),
                    ("d", 9, 10),
                ],
            );
            link(tx, "r", "a.tx", a[1], "calls", a[0]);
            link(tx, "r", "a.tx", a[2], "calls", a[0]);
            link_conf(tx, "r", "a.tx", a[3], "calls", a[0], Confidence::Inferred);
            link(tx, "r", "a.tx", a[4], "references", a[0]);
            // Structure and verification links are never followed.
            link(tx, "r", "a.tx", a[4], "contains", a[0]);
        });
        let r = run(
            &c,
            &[seed(Id::of("r:a.tx#fn:t"))],
            &ImpactOptions::default(),
        );
        let shape: Vec<_> = r
            .groups
            .iter()
            .map(|g| (g.kind.as_str(), g.confidence, g.hits.len()))
            .collect();
        assert_eq!(
            shape,
            [
                ("calls", Confidence::Extracted, 2),
                ("references", Confidence::Extracted, 1),
                ("calls", Confidence::Inferred, 1),
            ]
        );
    }

    #[test]
    fn output_is_deterministic() {
        let fill = |order: &[usize]| {
            let order = order.to_vec();
            build(&["r"], move |tx| {
                let (_, a) = artifact(
                    tx,
                    "r",
                    "a.tx",
                    &[("t", 1, 2), ("a", 3, 4), ("b", 5, 6), ("c", 7, 8)],
                );
                let links = [
                    (1usize, 0usize, "calls"),
                    (2, 0, "calls"),
                    (3, 1, "references"),
                ];
                for i in order {
                    let (f, t, k) = links[i];
                    link(tx, "r", "a.tx", a[f], k, a[t]);
                }
            })
        };
        let (_t1, c1) = fill(&[0, 1, 2]);
        let (_t2, c2) = fill(&[2, 1, 0]);
        let s = [seed(Id::of("r:a.tx#fn:t"))];
        let j1 = serde_json::to_string(&run(&c1, &s, &ImpactOptions::default())).unwrap();
        let j2 = serde_json::to_string(&run(&c2, &s, &ImpactOptions::default())).unwrap();
        assert_eq!(j1, j2);
    }

    #[test]
    fn tests_listed_not_in_groups() {
        let (_t, c) = build(&["r"], |tx| {
            let (_, a) = artifact(
                tx,
                "r",
                "a.tx",
                &[("t", 1, 2), ("user", 3, 4), ("check", 5, 6)],
            );
            link(tx, "r", "a.tx", a[1], "calls", a[0]);
            link(tx, "r", "a.tx", a[2], "calls", a[0]);
            mark_test(tx, a[2], "case");
        });
        let r = run(
            &c,
            &[seed(Id::of("r:a.tx#fn:t"))],
            &ImpactOptions::default(),
        );
        assert_eq!(all_hits(&r), ["r:a.tx#fn:user"]);
        assert_eq!(r.total, Some(1));
        assert_eq!(r.tests.len(), 1);
        assert_eq!(r.tests[0].test.name, "r:a.tx#fn:check");
    }

    fn fan(n: usize) -> (tempfile::TempDir, Cache) {
        build(&["r"], |tx| {
            let names: Vec<String> = (0..n).map(|i| format!("u{i:02}")).collect();
            let mut syms: Vec<(&str, u32, u32)> = vec![("t", 1, 2)];
            for (i, nm) in names.iter().enumerate() {
                syms.push((nm.as_str(), 10 + i as u32, 10 + i as u32));
            }
            let (_, a) = artifact(tx, "r", "a.tx", &syms);
            for u in &a[1..] {
                link(tx, "r", "a.tx", *u, "calls", a[0]);
            }
        })
    }

    #[test]
    fn node_cap_reports_partial() {
        let (_t, c) = fan(10);
        let s = [seed(Id::of("r:a.tx#fn:t"))];
        let opts = ImpactOptions {
            limit: 3,
            ..Default::default()
        };
        let r = run(&c, &s, &opts);
        assert_eq!(all_hits(&r).len(), 3);
        assert_eq!(r.total, Some(10));
        assert_eq!(r.partial.len(), 1);
        let p = &r.partial[0];
        assert_eq!(
            (p.cap.as_str(), p.at, p.shown, p.total),
            (cap::LIMIT, 3, 3, Some(10))
        );
        // The same three whatever the limit's source: the first by name.
        assert_eq!(
            all_hits(&r),
            ["r:a.tx#fn:u00", "r:a.tx#fn:u01", "r:a.tx#fn:u02"]
        );
        // A runaway walk stops: the total is unknown.
        let opts = ImpactOptions {
            limit: 1,
            ..Default::default()
        };
        let r = run(&c, &s, &opts);
        assert_eq!(r.total, None);
        assert_eq!(r.partial[0].total, None);
        assert_eq!(all_hits(&r).len(), 1);
    }

    #[test]
    fn depth_cap_reports_partial() {
        let (_t, c) = build(&["r"], |tx| {
            let (_, a) = artifact(
                tx,
                "r",
                "a.tx",
                &[("t", 1, 1), ("n1", 2, 2), ("n2", 3, 3), ("n3", 4, 4)],
            );
            for i in 1..4 {
                link(tx, "r", "a.tx", a[i], "calls", a[i - 1]);
            }
        });
        let s = [seed(Id::of("r:a.tx#fn:t"))];
        let opts = ImpactOptions {
            depth: 2,
            ..Default::default()
        };
        let r = run(&c, &s, &opts);
        assert_eq!(all_hits(&r), ["r:a.tx#fn:n1", "r:a.tx#fn:n2"]);
        assert_eq!(r.partial.len(), 1);
        let p = &r.partial[0];
        assert_eq!(
            (p.cap.as_str(), p.at, p.shown, p.total),
            (cap::DEPTH, 2, 2, None)
        );
        let r = run(&c, &s, &ImpactOptions::default());
        assert_eq!(all_hits(&r).len(), 3);
        assert!(r.partial.is_empty());
    }

    #[test]
    fn open_contract_listed_as_boundary() {
        let (_t, c) = build(&["r"], |tx| {
            let (_, f) = artifact(tx, "r", "f.tx", &[("only_provider", 1, 2)]);
            let k = contract(tx, "channel", "lonely", 1, 0, 0);
            link(tx, "r", "f.tx", f[0], "provides", k);
        });
        let r = run(
            &c,
            &[seed(Id::of("r:f.tx#fn:only_provider"))],
            &ImpactOptions::default(),
        );
        assert!(r.groups.is_empty());
        assert_eq!(r.contracts.len(), 1);
        assert_eq!(r.contracts[0].open.as_deref(), Some("no-consumer"));
        assert_eq!((r.contracts[0].providers, r.contracts[0].consumers), (1, 0));
    }
}
