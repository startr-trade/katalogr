//! Semantic diff over hand-built caches (FEAT-095 R-3 to R-7).

mod semantic_diff {
    use camino::Utf8PathBuf;
    use katalogr_analytics::context_diff::context_diff;
    use katalogr_analytics::semantic_diff::{
        ContractCounts, DiffOptions, SemanticDiff, SymbolRef, semantic_diff,
    };
    use katalogr_model::{
        ArtifactName, Confidence, ContractName, ContractRole, Fact, Id, LinkKind, Name, Provenance,
        RelPath, Segment, Span, SymbolName,
    };
    use katalogr_store::{
        ArtifactRow, BindingRow, Cache, ContractRow, Kindex, LinkEntry, LinkKey, LinkOrigin,
        LinkRow, ProjectConfig, ProjectName, RepoConfig, SymbolRow,
    };
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    /// A graph being described, then written to a temporary cache.
    #[derive(Default)]
    struct G {
        files: BTreeMap<String, String>,
        symbols: Vec<SymbolRow>,
        contracts: BTreeMap<Id, ContractRow>,
        bindings: BTreeMap<Id, Vec<BindingRow>>,
        links: Vec<LinkKey>,
    }

    struct Built {
        _tmp: tempfile::TempDir,
        cache: Cache,
    }

    fn aname(path: &str) -> ArtifactName {
        ArtifactName {
            repo: "r".into(),
            path: RelPath::new(path).unwrap(),
        }
    }

    fn sname(path: &str, name: &str) -> SymbolName {
        SymbolName {
            artifact: aname(path),
            segments: vec![Segment {
                kind: "fn".into(),
                name: name.into(),
            }],
            ordinal: 1,
        }
    }

    impl G {
        /// A file with the given revision text; its blob is the text up to the first `+`.
        fn file(&mut self, path: &str, revision: &str) -> &mut G {
            self.files.insert(path.into(), revision.into());
            self
        }

        fn sym(&mut self, path: &str, kind: &str, name: &str, signature: Option<&str>) -> Id {
            let n = sname(path, name);
            let id = n.id();
            let mut facts = BTreeMap::new();
            if let Some(s) = signature {
                facts.insert("signature".to_string(), Fact::Str(s.to_string()));
            }
            self.symbols.push(SymbolRow {
                name: n,
                artifact: aname(path).id(),
                kind: kind.into(),
                span: Span {
                    start_line: 3,
                    start_col: 1,
                    end_line: 3,
                    end_col: 2,
                },
                facts,
            });
            id
        }

        fn bind(&mut self, path: &str, sym: Id, role: ContractRole, kind: &str, key: &str) {
            let name = ContractName {
                kind: kind.into(),
                key: key.into(),
            };
            let cid = name.id();
            let row = self.contracts.entry(cid).or_insert(ContractRow {
                name,
                providers: 0,
                consumers: 0,
                declarers: 0,
            });
            match role {
                ContractRole::Provides => row.providers += 1,
                ContractRole::Consumes => row.consumers += 1,
                ContractRole::Declares => row.declarers += 1,
            }
            self.bindings
                .entry(aname(path).id())
                .or_default()
                .push(BindingRow {
                    symbol: sym,
                    contract: cid,
                    role,
                });
        }

        fn link(&mut self, from: Id, kind: &str, to: Id) {
            self.links.push(LinkKey {
                from,
                kind: LinkKind(kind.into()),
                to,
            });
        }

        fn build(&self) -> Built {
            let tmp = tempfile::tempdir().unwrap();
            let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
            let dir = Kindex::init(&root)
                .unwrap()
                .create_project(
                    &ProjectName::new("t").unwrap(),
                    ProjectConfig {
                        repos: vec![RepoConfig::new("r")],
                        ..Default::default()
                    },
                )
                .unwrap();
            let cache = Cache::open(&dir, Instant::now() + Duration::from_secs(60)).unwrap();
            let mut tx = cache.write().unwrap();
            for (path, revision) in &self.files {
                let name = aname(path);
                let mut symbols: Vec<Id> = self
                    .symbols
                    .iter()
                    .filter(|s| s.name.artifact == name)
                    .map(|s| s.name.id())
                    .collect();
                symbols.sort();
                tx.put_artifact(
                    name.id(),
                    &ArtifactRow {
                        provenance: Provenance {
                            repo: "r".into(),
                            path: name.path.clone(),
                            revision: revision.clone(),
                        },
                        name: name.clone(),
                        extension: "test".into(),
                        facts: BTreeMap::new(),
                        symbols,
                        exports: Vec::new(),
                        refs: Vec::new(),
                    },
                )
                .unwrap();
            }
            for s in &self.symbols {
                tx.put_symbol(s.name.id(), s).unwrap();
            }
            for (id, row) in &self.contracts {
                tx.put_contract(*id, row).unwrap();
            }
            for (aid, rows) in &self.bindings {
                tx.put_bindings(*aid, rows).unwrap();
            }
            for key in &self.links {
                tx.put_link(
                    key,
                    &LinkRow {
                        entries: vec![LinkEntry {
                            origin: LinkOrigin::Intra,
                            confidence: Confidence::Extracted,
                            span: None,
                            provenance: Provenance {
                                repo: "r".into(),
                                path: RelPath::new("x.tx").unwrap(),
                                revision: "r1".into(),
                            },
                            fanout: None,
                            produced_by: None,
                        }],
                    },
                )
                .unwrap();
            }
            tx.commit().unwrap();
            Built { _tmp: tmp, cache }
        }
    }

    fn opts(limit: usize) -> DiffOptions {
        DiffOptions {
            limit,
            exclude_path_prefixes: vec!["@history/".into()],
        }
    }

    fn run(a: &G, b: &G, limit: usize) -> SemanticDiff {
        let (ba, bb) = (a.build(), b.build());
        semantic_diff(
            &ba.cache.read().unwrap(),
            &bb.cache.read().unwrap(),
            &opts(limit),
        )
        .unwrap()
    }

    fn names(v: &[SymbolRef]) -> Vec<&str> {
        v.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn added_and_removed_symbols() {
        let mut a = G::default();
        a.file("one.rs", "r1");
        a.sym("one.rs", "fn", "keep", None);
        a.sym("one.rs", "fn", "gone", None);
        a.file("old.rs", "r1");
        a.sym("old.rs", "fn", "lost", None);
        let mut b = G::default();
        b.file("one.rs", "r2");
        b.sym("one.rs", "fn", "keep", None);
        b.sym("one.rs", "fn", "fresh", None);
        b.file("new.rs", "r1");
        b.sym("new.rs", "fn", "born", None);
        let d = run(&a, &b, 100);
        assert_eq!(
            names(&d.symbols.added),
            ["r:new.rs#fn:born", "r:one.rs#fn:fresh"]
        );
        assert_eq!(
            names(&d.symbols.removed),
            ["r:old.rs#fn:lost", "r:one.rs#fn:gone"]
        );
        assert_eq!((d.symbols.added_total, d.symbols.removed_total), (2, 2));
        assert_eq!(d.symbols.files_changed, 3);
        assert!(d.symbols.moved.is_empty() && d.symbols.changed.is_empty());
        assert_eq!(d.symbols.added[0].path, "new.rs");
        assert_eq!(d.symbols.added[0].line, 3);
        assert!(d.partial.is_empty());
    }

    #[test]
    fn moved_symbol_is_unique_match() {
        let mut a = G::default();
        a.file("a.rs", "r1").file("b.rs", "r1");
        a.sym("a.rs", "fn", "Foo", None);
        let mut b = G::default();
        b.file("a.rs", "r2").file("b.rs", "r2");
        b.sym("b.rs", "fn", "Foo", None);
        let d = run(&a, &b, 100);
        assert_eq!(d.symbols.moved.len(), 1);
        assert_eq!(d.symbols.moved_total, 1);
        assert_eq!(d.symbols.moved[0].from.path, "a.rs");
        assert_eq!(d.symbols.moved[0].to.path, "b.rs");
        assert!(d.symbols.added.is_empty() && d.symbols.removed.is_empty());
    }

    #[test]
    fn ambiguous_move_stays_added_and_removed() {
        let mut a = G::default();
        a.file("a.rs", "r1").file("c.rs", "r1").file("b.rs", "r1");
        a.sym("a.rs", "fn", "Foo", None);
        a.sym("c.rs", "fn", "Foo", None);
        let mut b = G::default();
        b.file("a.rs", "r2").file("c.rs", "r2").file("b.rs", "r2");
        b.sym("b.rs", "fn", "Foo", None);
        let d = run(&a, &b, 100);
        assert!(d.symbols.moved.is_empty());
        assert_eq!(d.symbols.added.len(), 1);
        assert_eq!(d.symbols.removed.len(), 2);
    }

    #[test]
    fn changed_signature_and_kind() {
        let mut a = G::default();
        a.file("a.rs", "r1");
        a.sym("a.rs", "fn", "sig", Some("fn sig(x: u8)"));
        a.sym("a.rs", "fn", "same", Some("fn same()"));
        a.sym("a.rs", "fn", "nosig", None);
        let mut b = G::default();
        b.file("a.rs", "r2");
        b.sym("a.rs", "fn", "sig", Some("fn sig(x: u16)"));
        b.sym("a.rs", "fn", "same", Some("fn same()"));
        b.sym("a.rs", "fn", "nosig", Some("fn nosig()"));
        let d = run(&a, &b, 100);
        assert_eq!(d.symbols.changed_total, 2);
        let c = &d.symbols.changed;
        assert_eq!(c[0].symbol.name, "r:a.rs#fn:nosig");
        assert_eq!(c[0].signature_a, None);
        assert_eq!(c[0].signature_b.as_deref(), Some("fn nosig()"));
        assert_eq!(c[1].symbol.name, "r:a.rs#fn:sig");
        assert_eq!(c[1].signature_a.as_deref(), Some("fn sig(x: u8)"));
        assert_eq!(c[1].signature_b.as_deref(), Some("fn sig(x: u16)"));
        assert_eq!(c[1].kind_a, "fn");

        // A kind change on the same symbol id.
        let mut a = G::default();
        a.file("a.rs", "r1");
        a.sym("a.rs", "fn", "k", None);
        let mut b = G::default();
        b.file("a.rs", "r2");
        b.sym("a.rs", "method", "k", None);
        let d = run(&a, &b, 100);
        assert_eq!(d.symbols.changed.len(), 1);
        assert_eq!(d.symbols.changed[0].kind_a, "fn");
        assert_eq!(d.symbols.changed[0].symbol.kind, "method");
    }

    #[test]
    fn unchanged_blob_reads_no_symbols() {
        // The file has the same blob on both sides (a `+` suffix is not part of it), so its
        // symbols are never read: a difference in them is not reported.
        let mut a = G::default();
        a.file("same.rs", "abc");
        a.sym("same.rs", "fn", "old", None);
        let mut b = G::default();
        b.file("same.rs", "abc+dirty");
        b.sym("same.rs", "fn", "new", None);
        let d = run(&a, &b, 100);
        assert_eq!(d.symbols.files_changed, 0);
        assert!(d.symbols.added.is_empty() && d.symbols.removed.is_empty());
        assert_eq!(d.symbols.added_total + d.symbols.removed_total, 0);
    }

    #[test]
    fn excluded_prefix_left_out() {
        let mut a = G::default();
        a.file("a.rs", "r1");
        let mut b = G::default();
        b.file("a.rs", "r1").file("@history/abc", "h1");
        b.sym("@history/abc", "commit", "abc", None);
        let d = run(&a, &b, 100);
        assert!(d.symbols.added.is_empty());
        assert_eq!(d.symbols.files_changed, 0);
    }

    #[test]
    fn reuses_context_diff_lists() {
        let mut a = G::default();
        a.file("a.rs", "r1");
        let x = a.sym("a.rs", "fn", "x", None);
        let y = a.sym("a.rs", "fn", "y", None);
        a.link(x, "calls", y);
        let mut b = G::default();
        b.file("a.rs", "r1");
        let x = b.sym("a.rs", "fn", "x", None);
        let y = b.sym("a.rs", "fn", "y", None);
        b.link(y, "calls", x);
        let (ba, bb) = (a.build(), b.build());
        let (ra, rb) = (ba.cache.read().unwrap(), bb.cache.read().unwrap());
        let d = semantic_diff(&ra, &rb, &opts(100)).unwrap();
        let mut direct = context_diff(&ra, &rb, 100).unwrap();
        // `partial` moves to the report's own list.
        let moved_partial = std::mem::take(&mut direct.partial);
        assert_eq!(d.links, direct);
        assert_eq!(d.links.links_added.len(), 1);
        assert_eq!(d.links.links_removed.len(), 1);
        assert_eq!(d.partial, moved_partial);
    }

    fn two_sided(build: impl Fn(&mut G, bool)) -> (G, G) {
        let (mut a, mut b) = (G::default(), G::default());
        build(&mut a, false);
        build(&mut b, true);
        (a, b)
    }

    #[test]
    fn new_consumer_reported() {
        let (a, b) = two_sided(|g, after| {
            g.file("p.rs", "r1").file("c.rs", "r1");
            let p = g.sym("p.rs", "fn", "serve", None);
            g.bind("p.rs", p, ContractRole::Provides, "route", "x");
            let c1 = g.sym("c.rs", "fn", "first", None);
            g.bind("c.rs", c1, ContractRole::Consumes, "route", "x");
            if after {
                let c2 = g.sym("c.rs", "fn", "second", None);
                g.bind("c.rs", c2, ContractRole::Consumes, "route", "x");
            }
        });
        let d = run(&a, &b, 100);
        assert_eq!(d.contracts_total, 1);
        let c = &d.contracts[0];
        assert_eq!(c.contract, "contract:route:x");
        assert_eq!(c.consumers_added, ["r:c.rs#fn:second"]);
        assert!(c.consumers_removed.is_empty() && c.providers_added.is_empty());
        assert_eq!(
            (c.a, c.b),
            (
                ContractCounts {
                    providers: 1,
                    consumers: 1,
                    declarers: 0
                },
                ContractCounts {
                    providers: 1,
                    consumers: 2,
                    declarers: 0
                }
            )
        );
        assert!(!c.newly_dangling && !c.no_longer_dangling);
    }

    #[test]
    fn contract_becomes_dangling() {
        let (a, b) = two_sided(|g, after| {
            g.file("p.rs", "r1").file("c.rs", "r1");
            let c = g.sym("c.rs", "fn", "call", None);
            g.bind("c.rs", c, ContractRole::Consumes, "route", "GET /y");
            if !after {
                let p = g.sym("p.rs", "fn", "serve", None);
                g.bind("p.rs", p, ContractRole::Provides, "route", "GET /y");
            }
        });
        let d = run(&a, &b, 100);
        let c = &d.contracts[0];
        assert!(c.newly_dangling);
        assert!(!c.no_longer_dangling);
        assert_eq!(c.providers_removed, ["r:p.rs#fn:serve"]);
    }

    #[test]
    fn contract_stops_dangling() {
        let (a, b) = two_sided(|g, after| {
            g.file("p.rs", "r1").file("c.rs", "r1");
            let c = g.sym("c.rs", "fn", "call", None);
            g.bind("c.rs", c, ContractRole::Consumes, "route", "GET /y");
            if after {
                let p = g.sym("p.rs", "fn", "serve", None);
                g.bind("p.rs", p, ContractRole::Provides, "route", "GET /y");
            }
        });
        let d = run(&a, &b, 100);
        let c = &d.contracts[0];
        assert!(c.no_longer_dangling);
        assert!(!c.newly_dangling);
        assert_eq!(c.providers_added, ["r:p.rs#fn:serve"]);
    }

    #[test]
    fn capped_lists_are_partial() {
        let a = {
            let mut g = G::default();
            g.file("a.rs", "r1");
            g
        };
        let mut b = G::default();
        b.file("a.rs", "r2");
        for i in 0..5 {
            b.sym("a.rs", "fn", &format!("n{i}"), None);
        }
        let d = run(&a, &b, 2);
        assert_eq!(d.symbols.added.len(), 2);
        assert_eq!(d.symbols.added_total, 5);
        assert_eq!(d.partial.len(), 1);
        let p = &d.partial[0];
        assert_eq!(
            (p.cap.as_str(), p.at, p.shown, p.total),
            ("limit", 2, 2, Some(5))
        );
        // The same diff with room has no marker.
        assert!(run(&a, &b, 5).partial.is_empty());
    }

    // ---- insight shifts (R-6) ----

    use katalogr_analytics::insight_shift::{InsightSide, insight_shifts};
    use katalogr_analytics::metrics::{Better, MetricSample};
    use katalogr_analytics::{
        Community, CommunityReport, CoverageReport, Cycle, CycleReport, HubEntry, HubReport, Level,
        NodeHit,
    };

    fn node(name: &str) -> NodeHit {
        NodeHit {
            level: Level::File,
            name: name.into(),
            kind: "artifact".into(),
            path: Some(format!("{name}.rs")),
            line: None,
        }
    }

    fn hub(name: &str, degree: f64, rank: f64) -> HubEntry {
        HubEntry {
            node: node(name),
            in_degree: degree,
            out_degree: 0.0,
            degree,
            z: 0.0,
            rank,
            members: 1,
        }
    }

    fn hubs(items: Vec<HubEntry>) -> HubReport {
        HubReport {
            level: Level::File,
            nodes: items.len() as u64,
            mean_degree: 1.0,
            items,
        }
    }

    fn community(label: &str, size: u64) -> Community {
        Community {
            id: 0,
            size,
            label: node(label),
            dir: Some(format!("src/{label}")),
            cohesion: 0.5,
            internal_weight: 1.0,
            boundary_weight: 1.0,
            top: Vec::new(),
            bridges: Vec::new(),
            files: size,
        }
    }

    fn communities(items: Vec<Community>, modularity: f64) -> CommunityReport {
        CommunityReport {
            level: Level::File,
            seed: 1,
            resolution: 1.0,
            modularity,
            communities_total: items.len() as u64,
            unclustered: 0,
            note: None,
            items,
        }
    }

    fn cycle(members: &[&str]) -> Cycle {
        Cycle {
            id: 0,
            size: members.len() as u64,
            members: members.iter().map(|m| node(m)).collect(),
            members_total: members.len() as u64,
            witness: Vec::new(),
            confidence: Confidence::Extracted,
        }
    }

    fn cycles(items: Vec<Cycle>) -> CycleReport {
        CycleReport {
            level: Level::File,
            include_inferred: false,
            cycles_total: items.len() as u64,
            cyclic_nodes: items.iter().map(|c| c.size).sum(),
            skipped_contracts: 0,
            items,
        }
    }

    fn coverage(samples: Vec<MetricSample>) -> CoverageReport {
        CoverageReport {
            totals: Default::default(),
            by_repo: Vec::new(),
            by_extension: Vec::new(),
            by_language: Vec::new(),
            groups: Vec::new(),
            by_link_kind: Vec::new(),
            dangling_contracts_by_kind: Vec::new(),
            unclaimed: Vec::new(),
            roots_missing: Vec::new(),
            metrics: samples,
        }
    }

    struct Reports {
        hubs: HubReport,
        communities: CommunityReport,
        cycles: CycleReport,
        coverage: CoverageReport,
    }

    impl Reports {
        fn empty() -> Reports {
            Reports {
                hubs: hubs(vec![]),
                communities: communities(vec![], 0.0),
                cycles: cycles(vec![]),
                coverage: coverage(vec![]),
            }
        }
        fn side(&self) -> InsightSide<'_> {
            InsightSide {
                hubs: &self.hubs,
                communities: &self.communities,
                cycles: &self.cycles,
                coverage: &self.coverage,
            }
        }
    }

    #[test]
    fn hub_shift_sorted_by_change() {
        let mut a = Reports::empty();
        let mut b = Reports::empty();
        a.hubs = hubs(vec![
            hub("same", 5.0, 0.1),
            hub("small", 4.0, 0.1),
            hub("gone", 6.0, 0.2),
        ]);
        b.hubs = hubs(vec![
            hub("same", 5.0, 0.1),
            hub("small", 5.0, 0.1),
            hub("big", 9.0, 0.3),
        ]);
        let mut partial = Vec::new();
        let s = insight_shifts(&a.side(), &b.side(), 10, &mut partial);
        let order: Vec<(&str, f64, f64)> = s
            .hubs
            .iter()
            .map(|h| (h.name.as_str(), h.degree_a, h.degree_b))
            .collect();
        // Unchanged is dropped; a side where it is absent counts as 0; largest change first.
        assert_eq!(
            order,
            [("big", 0.0, 9.0), ("gone", 6.0, 0.0), ("small", 4.0, 5.0)]
        );
        assert!(partial.is_empty());
        // A cut is marked.
        let mut partial = Vec::new();
        let s = insight_shifts(&a.side(), &b.side(), 1, &mut partial);
        assert_eq!(s.hubs.len(), 1);
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].total, Some(3));
    }

    #[test]
    fn cycle_added_and_removed() {
        let mut a = Reports::empty();
        let mut b = Reports::empty();
        a.cycles = cycles(vec![cycle(&["x", "y"]), cycle(&["p", "q", "r"])]);
        b.cycles = cycles(vec![cycle(&["y", "x"]), cycle(&["m", "n"])]);
        let mut partial = Vec::new();
        let s = insight_shifts(&a.side(), &b.side(), 10, &mut partial);
        assert_eq!(s.cycles.added, [vec!["m".to_string(), "n".to_string()]]);
        assert_eq!(
            s.cycles.removed,
            [vec!["p".to_string(), "q".to_string(), "r".to_string()]]
        );
        assert_eq!((s.cycles.total_a, s.cycles.total_b), (2, 2));
        assert_eq!((s.cycles.cyclic_nodes_a, s.cycles.cyclic_nodes_b), (5, 4));
    }

    #[test]
    fn community_appears_and_resizes() {
        let mut a = Reports::empty();
        let mut b = Reports::empty();
        a.communities = communities(
            vec![
                community("core", 10),
                community("old", 3),
                community("flat", 4),
            ],
            0.31,
        );
        b.communities = communities(
            vec![
                community("core", 12),
                community("fresh", 5),
                community("flat", 4),
            ],
            0.42,
        );
        let mut partial = Vec::new();
        let s = insight_shifts(&a.side(), &b.side(), 10, &mut partial);
        let c = &s.communities;
        assert_eq!(c.appeared.len(), 1);
        assert_eq!(c.appeared[0].label, "fresh");
        assert_eq!(c.appeared[0].size, 5);
        assert_eq!(c.disappeared[0].label, "old");
        assert_eq!(c.resized.len(), 1);
        assert_eq!(
            (
                c.resized[0].label.as_str(),
                c.resized[0].size_a,
                c.resized[0].size_b
            ),
            ("core", 10, 12)
        );
        assert_eq!((c.modularity_a, c.modularity_b), (0.31, 0.42));
        assert_eq!((c.total_a, c.total_b), (3, 3));
    }

    #[test]
    fn coverage_metric_deltas() {
        let mut a = Reports::empty();
        let mut b = Reports::empty();
        a.coverage = coverage(vec![
            MetricSample::new("coverage.resolution_rate", 0.5, Better::Higher),
            MetricSample::new("coverage.resolution_rate", 0.4, Better::Higher).label("repo", "r"),
            MetricSample::new("coverage.same", 1.0, Better::Info),
            MetricSample::new("other.metric", 1.0, Better::Info),
        ]);
        b.coverage = coverage(vec![
            MetricSample::new("coverage.resolution_rate", 0.75, Better::Higher),
            MetricSample::new("coverage.resolution_rate", 0.4, Better::Higher).label("repo", "r"),
            MetricSample::new("coverage.same", 1.0, Better::Info),
            MetricSample::new("other.metric", 2.0, Better::Info),
        ]);
        let mut partial = Vec::new();
        let s = insight_shifts(&a.side(), &b.side(), 10, &mut partial);
        assert_eq!(s.coverage.len(), 1);
        let m = &s.coverage[0];
        assert_eq!(m.name, "coverage.resolution_rate");
        assert_eq!((m.a, m.b, m.delta), (0.5, 0.75, 0.25));
        assert_eq!(m.better, Better::Higher);
    }
}
