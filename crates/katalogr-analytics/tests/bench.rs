//! Timing benchmarks, run on demand:
//! `cargo test -p katalogr-analytics --release --test bench -- --ignored --nocapture --test-threads=1`.
//!
//! The graph has 100,000 `fn` symbols in 1,000 files of 20 packages and 300,000 links; the rank
//! is stored as the processor stores it at publish. `coverage_100k` has its own cache: 300,000
//! references in 1,000 artifacts.

mod bench {
    use camino::Utf8PathBuf;
    use katalogr_analytics::{
        CommunityOptions, CycleOptions, DEFAULT_SEED, DeadCodeOptions, DeadConfidence,
        GraphOptions, HubOptions, Level, RationaleScope, STRUCTURAL_KINDS, communities_with_stats,
        cycles, dead_code, hubs, rationale,
    };
    use katalogr_model::{
        ArtifactName, Confidence, Fact, Fragment, Id, LinkKind, Name, Provenance, RelPath, Segment,
        Span, SymbolName,
    };
    use katalogr_store::{
        ArtifactRow, Cache, Kindex, LinkEntry, LinkKey, LinkOrigin, LinkRow, ProjectConfig,
        ProjectName, Reader, RepoConfig, Restitch, StoreError, SymbolRow, WriteTx,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    const SYMBOLS: usize = 100_000;
    const FILES: usize = 1_000;
    const PER_FILE: usize = SYMBOLS / FILES;
    const PACKAGES: usize = 20;
    const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

    /// The bench graph has no shards, so restitching never runs.
    struct NoShards;

    impl Restitch for NoShards {
        fn replace_fragment(&self, _: &mut WriteTx, _: Fragment) -> Result<(), StoreError> {
            Ok(())
        }
        fn remove_artifact(&self, _: &mut WriteTx, _: &ArtifactName) -> Result<(), StoreError> {
            Ok(())
        }
    }

    /// xorshift64: a small deterministic generator; `state` must start non-zero.
    fn next_rand(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    struct Bench {
        _tmp: tempfile::TempDir,
        cache: Cache,
    }

    /// Build the graph once for all benchmarks.
    fn bench_cache() -> &'static Bench {
        static CACHE: OnceLock<Bench> = OnceLock::new();
        CACHE.get_or_init(build)
    }

    fn build() -> Bench {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("bench").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let cache =
            Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60))
                .unwrap();
        let span = Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
        };
        let provenance = |path: &RelPath| Provenance {
            repo: "r".into(),
            path: path.clone(),
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        };

        let mut tx = cache.write().unwrap();
        let mut ids: Vec<Id> = Vec::with_capacity(SYMBOLS);
        let mut paths: Vec<RelPath> = Vec::with_capacity(FILES);
        for f in 0..FILES {
            let path = RelPath::new(&format!("dir{}/f{f}.tx", f % 50)).unwrap();
            let artifact = ArtifactName {
                repo: "r".into(),
                path: path.clone(),
            };
            let artifact_id = artifact.id();
            let mut symbols = Vec::with_capacity(PER_FILE);
            for k in 0..PER_FILE {
                let i = f * PER_FILE + k;
                let name = SymbolName {
                    artifact: artifact.clone(),
                    segments: vec![Segment {
                        kind: "fn".into(),
                        name: format!("s{i}"),
                    }],
                    ordinal: 1,
                };
                let id = name.id();
                let mut facts = BTreeMap::new();
                if i.is_multiple_of(10) {
                    facts.insert("root".to_string(), Fact::Str("main".into()));
                }
                if i.is_multiple_of(100) {
                    facts.insert(
                        "rationale".to_string(),
                        Fact::List(vec![Fact::Map(BTreeMap::from([
                            ("marker".to_string(), Fact::Str("WHY".into())),
                            ("line".to_string(), Fact::Int(1)),
                            ("text".to_string(), Fact::Str(format!("reason {i}"))),
                        ]))]),
                    );
                }
                tx.put_symbol(
                    id,
                    &SymbolRow {
                        name,
                        artifact: artifact_id,
                        kind: "fn".into(),
                        span,
                        facts,
                    },
                )
                .unwrap();
                ids.push(id);
                symbols.push(id);
            }
            symbols.sort();
            tx.put_artifact(
                artifact_id,
                &ArtifactRow {
                    name: artifact,
                    extension: "test".into(),
                    provenance: provenance(&path),
                    facts: BTreeMap::from([(
                        "package".to_string(),
                        Fact::Str(format!("p{}", f % PACKAGES)),
                    )]),
                    symbols,
                    exports: Vec::new(),
                    refs: Vec::new(),
                },
            )
            .unwrap();
            paths.push(path);
        }

        // Each symbol calls two symbols of its own file and one anywhere; every 50th link is
        // inferred with a fan-out of three.
        let mut rng = SEED;
        let mut count = 0usize;
        for (i, from) in ids.iter().enumerate() {
            let file = i / PER_FILE;
            let mut targets: Vec<usize> = Vec::with_capacity(3);
            while targets.len() < 2 {
                let j = file * PER_FILE + next_rand(&mut rng) as usize % PER_FILE;
                if j != i && !targets.contains(&j) {
                    targets.push(j);
                }
            }
            while targets.len() < 3 {
                let j = next_rand(&mut rng) as usize % SYMBOLS;
                if j != i && !targets.contains(&j) {
                    targets.push(j);
                }
            }
            for j in targets {
                count += 1;
                let inferred = count.is_multiple_of(50);
                tx.put_link(
                    &LinkKey {
                        from: *from,
                        kind: LinkKind("calls".into()),
                        to: ids[j],
                    },
                    &LinkRow {
                        entries: vec![LinkEntry {
                            origin: LinkOrigin::Intra,
                            confidence: if inferred {
                                Confidence::Inferred
                            } else {
                                Confidence::Extracted
                            },
                            span: Some(span),
                            provenance: provenance(&paths[file]),
                            fanout: inferred.then_some(3),
                            produced_by: None,
                        }],
                    },
                )
                .unwrap();
            }
        }
        tx.commit().unwrap();

        // The publish step: compute the rank once and store it.
        let mut tx = cache.write().unwrap();
        let symbols = tx.symbols().unwrap();
        let scores = katalogr_store::pagerank(&tx, &symbols).unwrap();
        let stored: Vec<(Id, f64)> = symbols.iter().map(|(i, _)| *i).zip(scores).collect();
        let wm = tx.watermark(katalogr_store::RANK_CONTEXT).unwrap();
        tx.store_rank(&stored, wm).unwrap();
        tx.commit().unwrap();
        Bench { _tmp: tmp, cache }
    }

    fn graph(level: Level, inferred: bool) -> GraphOptions {
        GraphOptions::new(level, inferred)
    }

    fn report(name: &str, started: Instant, budget_ms: u64) {
        let elapsed = started.elapsed();
        println!("bench::{name}: {} ms", elapsed.as_secs_f64() * 1000.0);
        assert!(
            elapsed < Duration::from_millis(budget_ms),
            "{name} over its {budget_ms} ms budget"
        );
    }

    #[test]
    #[ignore]
    fn hubs_100k() {
        let read = bench_cache().cache.read().unwrap();
        let opts = HubOptions {
            graph: graph(Level::Symbol, true),
            min_degree: 10.0,
            limit: 20,
        };
        let start = Instant::now();
        let r = hubs(&read, &opts).unwrap();
        report("hubs_100k", start, 1000);
        assert_eq!(r.nodes, SYMBOLS as u64);
        println!(
            "hubs: mean degree {}, {} hubs listed at degree >= 10",
            r.mean_degree,
            r.items.len()
        );
        assert!(!r.items.is_empty());
    }

    #[test]
    #[ignore]
    fn cycles_100k() {
        let read = bench_cache().cache.read().unwrap();
        for (level, label) in [(Level::File, "file"), (Level::Symbol, "symbol")] {
            let opts = CycleOptions {
                graph: graph(level, false),
                members: 10,
                limit: 20,
            };
            let start = Instant::now();
            let r = cycles(&read, &opts).unwrap();
            report(&format!("cycles_100k ({label})"), start, 1000);
            println!(
                "cycles {label}: {} cycles, {} nodes in cycles",
                r.cycles_total, r.cyclic_nodes
            );
            assert!(r.cycles_total >= 1);
        }
    }

    #[test]
    #[ignore]
    fn dead_code_100k() {
        let read = bench_cache().cache.read().unwrap();
        let opts = DeadCodeOptions {
            code_kinds: BTreeMap::from([("test".to_string(), BTreeSet::from(["fn".to_string()]))]),
            exclude_kinds: STRUCTURAL_KINDS.iter().map(|k| k.to_string()).collect(),
            internal: false,
            min_confidence: DeadConfidence::Low,
            path_prefix: None,
            limit: 50,
        };
        let start = Instant::now();
        let r = dead_code(&read, &opts).unwrap();
        report("dead_code_100k", start, 1000);
        println!(
            "dead code: {} roots, {} high, {} medium, {} low",
            r.roots, r.high, r.medium, r.low
        );
        assert_eq!(r.roots, (SYMBOLS / 10) as u64);
    }

    #[test]
    #[ignore]
    fn rationale_100k() {
        let read = bench_cache().cache.read().unwrap();
        let start = Instant::now();
        let r = rationale(&read, &RationaleScope::All, 100).unwrap();
        report("rationale_100k", start, 1000);
        assert_eq!(r.entries_total, (SYMBOLS / 100) as u64);
        assert_eq!(r.items.len(), 100);
    }

    #[test]
    #[ignore]
    fn communities_100k() {
        let read = bench_cache().cache.read().unwrap();
        for (level, label, budget_ms) in
            [(Level::File, "file", 1000), (Level::Symbol, "symbol", 3000)]
        {
            let opts = CommunityOptions {
                graph: graph(level, true),
                seed: DEFAULT_SEED,
                resolution: 1.0,
                min_size: 3,
                members: 5,
                limit: 20,
                of: None,
            };
            let start = Instant::now();
            let (r, stats) = communities_with_stats(&read, &opts).unwrap();
            report(&format!("communities_100k ({label})"), start, budget_ms);
            println!(
                "communities {label}: {} communities, modularity {}, {} levels, at most {} passes in a level",
                r.communities_total, r.modularity, stats.levels, stats.max_passes
            );
            assert!(r.communities_total >= 1);
        }
    }

    /// The coverage cache: 100,000 symbols in 1,000 artifacts, 3 references per symbol (80% with
    /// one target, 10% with two, 10% unresolved), 2,000 contracts of which 300 are one-sided,
    /// and 50 index-error rows.
    fn coverage_cache() -> &'static Bench {
        static CACHE: OnceLock<Bench> = OnceLock::new();
        CACHE.get_or_init(build_coverage)
    }

    fn build_coverage() -> Bench {
        use katalogr_model::{ContractName, ContractRole, ExportKey, OutRef};
        use katalogr_store::{BindingRow, ContractRow, ErrorRow, RefId};
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("bench").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let cache =
            Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60))
                .unwrap();
        let span = Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
        };
        let provenance = |path: &RelPath| Provenance {
            repo: "r".into(),
            path: path.clone(),
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        };
        let ids: Vec<Id> = (0..SYMBOLS)
            .map(|i| Id::of(&format!("r:cov#fn:s{i}")))
            .collect();
        let mut bindings: BTreeMap<usize, Vec<BindingRow>> = BTreeMap::new();
        let mut tx = cache.write().unwrap();
        let mut artifact_ids = Vec::with_capacity(FILES);
        let mut rng = SEED;
        for f in 0..FILES {
            let path = RelPath::new(&format!("dir{}/f{f}.tx", f % 50)).unwrap();
            let artifact = ArtifactName {
                repo: "r".into(),
                path: path.clone(),
            };
            let artifact_id = artifact.id();
            let mut refs = Vec::with_capacity(PER_FILE * 3);
            let mut symbols = Vec::with_capacity(PER_FILE);
            for k in 0..PER_FILE {
                let i = f * PER_FILE + k;
                let from = ids[i];
                symbols.push(from);
                tx.put_symbol(
                    from,
                    &SymbolRow {
                        name: SymbolName {
                            artifact: artifact.clone(),
                            segments: vec![Segment {
                                kind: "fn".into(),
                                name: format!("s{i}"),
                            }],
                            ordinal: 1,
                        },
                        artifact: artifact_id,
                        kind: "fn".into(),
                        span,
                        facts: BTreeMap::new(),
                    },
                )
                .unwrap();
                for r in 0..3 {
                    let index = refs.len() as u32;
                    let out = OutRef {
                        from,
                        kind: LinkKind("calls".into()),
                        span: Some(span),
                        candidates: vec![ExportKey {
                            namespace: "ns".into(),
                            key: format!("k{i}_{r}"),
                        }],
                        confidence: Confidence::Extracted,
                        origin: None,
                        binds: None,
                    };
                    refs.push(out.clone());
                    let roll = next_rand(&mut rng) % 10;
                    let targets = match roll {
                        0 => 0,
                        1 => 2,
                        _ => 1,
                    };
                    if targets == 0 {
                        tx.put_unresolved(
                            RefId {
                                artifact: artifact_id,
                                index,
                            },
                            &out,
                        )
                        .unwrap();
                    }
                    for t in 0..targets {
                        let to = ids[(i * 7 + r * 13 + t * 1009 + 1) % SYMBOLS];
                        tx.put_link(
                            &LinkKey {
                                from,
                                kind: LinkKind("calls".into()),
                                to,
                            },
                            &LinkRow {
                                entries: vec![LinkEntry {
                                    origin: LinkOrigin::Ref(index),
                                    confidence: if targets > 1 {
                                        Confidence::Inferred
                                    } else {
                                        Confidence::Extracted
                                    },
                                    span: Some(span),
                                    provenance: provenance(&path),
                                    fanout: (targets > 1).then_some(targets as u32),
                                    produced_by: None,
                                }],
                            },
                        )
                        .unwrap();
                    }
                }
            }
            symbols.sort();
            tx.put_artifact(
                artifact_id,
                &ArtifactRow {
                    name: artifact,
                    extension: "test".into(),
                    provenance: provenance(&path),
                    facts: BTreeMap::new(),
                    symbols,
                    exports: Vec::new(),
                    refs,
                },
            )
            .unwrap();
            tx.put_index_errors(
                "r",
                &path,
                &if f < 50 {
                    vec![ErrorRow {
                        span: None,
                        message: "parse error: x".into(),
                    }]
                } else {
                    Vec::new()
                },
            )
            .unwrap();
            artifact_ids.push(artifact_id);
        }
        for c in 0..2_000usize {
            let (providers, consumers) = match c {
                0..150 => (1, 0),
                150..300 => (0, 1),
                _ => (1, 1),
            };
            let id = Id::of(&format!("contract:wasm:c{c}"));
            tx.put_contract(
                id,
                &ContractRow {
                    name: ContractName {
                        kind: "wasm".into(),
                        key: format!("c{c}"),
                    },
                    providers,
                    consumers,
                    declarers: 0,
                },
            )
            .unwrap();
            let owner = c % FILES;
            let symbol = ids[owner * PER_FILE];
            let role = if providers > 0 {
                ContractRole::Provides
            } else {
                ContractRole::Consumes
            };
            bindings.entry(owner).or_default().push(BindingRow {
                symbol,
                contract: id,
                role,
            });
        }
        for (owner, rows) in &bindings {
            tx.put_bindings(artifact_ids[*owner], rows).unwrap();
        }
        tx.commit().unwrap();
        Bench { _tmp: tmp, cache }
    }

    #[test]
    #[ignore]
    fn coverage_100k() {
        use katalogr_analytics::{CoverageInput, coverage};
        let read = coverage_cache().cache.read().unwrap();
        let language = |ext: &str, _: &RelPath| ext.to_string();
        let claim = |_: &RelPath| None;
        let input = CoverageInput {
            language: &language,
            claim: &claim,
            unclaimed: BTreeMap::new(),
            roots_missing: Vec::new(),
            exclude_prefixes: vec!["@history/".into()],
            repo: None,
            suffix_top: 20,
        };
        let start = Instant::now();
        let r = coverage(&read, &input).unwrap();
        report("coverage_100k", start, 1500);
        println!(
            "coverage: {} refs ({:.1}% resolved, {:.1}% inferred, {:.1}% unresolved), {} dangling contracts",
            r.totals.refs_total,
            r.totals.resolution_rate.unwrap() * 100.0,
            r.totals.inferred_rate.unwrap() * 100.0,
            r.totals.unresolved_rate.unwrap() * 100.0,
            r.totals.contracts_dangling
        );
        assert_eq!(r.totals.refs_total, 300_000);
        assert_eq!(r.totals.files_indexed, FILES as u64);
        assert_eq!(r.totals.contracts_dangling, 300);
        assert_eq!(r.totals.files_parse_errors, 50);
    }

    /// A graph of 20,000 symbols in 100 files: one export key with 230 targets, 5,000 fan rows
    /// from distinct callers naming it (1.15 M expanded entries) and 30,000 extracted calls.
    fn build_fan() -> Bench {
        use katalogr_model::ExportKey;
        use katalogr_store::FanKey;
        const N: usize = 20_000;
        const FILES_F: usize = 100;
        const PER: usize = N / FILES_F;
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("bench").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let cache =
            Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60))
                .unwrap();
        let span = Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
        };
        let provenance = |path: &RelPath| Provenance {
            repo: "r".into(),
            path: path.clone(),
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        };
        let mut tx = cache.write().unwrap();
        let mut ids: Vec<Id> = Vec::with_capacity(N);
        let mut paths: Vec<RelPath> = Vec::new();
        for f in 0..FILES_F {
            let path = RelPath::new(&format!("dir{}/f{f}.tx", f % 10)).unwrap();
            let artifact = ArtifactName {
                repo: "r".into(),
                path: path.clone(),
            };
            let aid = artifact.id();
            let mut symbols = Vec::new();
            for k in 0..PER {
                let i = f * PER + k;
                let name = SymbolName {
                    artifact: artifact.clone(),
                    segments: vec![Segment {
                        kind: "fn".into(),
                        name: format!("s{i}"),
                    }],
                    ordinal: 1,
                };
                let id = name.id();
                let mut facts = BTreeMap::new();
                if i.is_multiple_of(10) {
                    facts.insert("root".to_string(), Fact::Str("main".into()));
                }
                tx.put_symbol(
                    id,
                    &SymbolRow {
                        name,
                        artifact: aid,
                        kind: "fn".into(),
                        span,
                        facts,
                    },
                )
                .unwrap();
                ids.push(id);
                symbols.push(id);
            }
            symbols.sort();
            tx.put_artifact(
                aid,
                &ArtifactRow {
                    name: artifact,
                    extension: "test".into(),
                    provenance: provenance(&path),
                    facts: BTreeMap::from([(
                        "package".to_string(),
                        Fact::Str(format!("p{}", f % 10)),
                    )]),
                    symbols,
                    exports: Vec::new(),
                    refs: Vec::new(),
                },
            )
            .unwrap();
            paths.push(path);
        }
        let entry =
            |origin: LinkOrigin, c: Confidence, fanout: Option<u32>, path: &RelPath| LinkEntry {
                origin,
                confidence: c,
                span: Some(span),
                provenance: provenance(path),
                fanout,
                produced_by: None,
            };
        for i in 0..N {
            let mut to = vec![(i + 1) % N];
            if i < 10_000 {
                to.push((i + 13) % N);
            }
            for j in to {
                tx.put_link(
                    &LinkKey {
                        from: ids[i],
                        kind: LinkKind("calls".into()),
                        to: ids[j],
                    },
                    &LinkRow {
                        entries: vec![entry(
                            LinkOrigin::Intra,
                            Confidence::Extracted,
                            None,
                            &paths[i / PER],
                        )],
                    },
                )
                .unwrap();
            }
        }
        let key = ExportKey {
            namespace: "m".into(),
            key: "area".into(),
        };
        let mut targets: Vec<Id> = ids[..230].to_vec();
        targets.sort();
        tx.put_exports(&key, &targets).unwrap();
        for i in 1000..6000 {
            tx.put_fan(
                &FanKey {
                    from: ids[i],
                    kind: LinkKind("calls".into()),
                    key: key.clone(),
                },
                &LinkRow {
                    entries: vec![entry(
                        LinkOrigin::Ref(0),
                        Confidence::Inferred,
                        None,
                        &paths[i / PER],
                    )],
                },
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let mut tx = cache.write().unwrap();
        let symbols = tx.symbols().unwrap();
        let scores = katalogr_store::pagerank(&tx, &symbols).unwrap();
        let stored: Vec<(Id, f64)> = symbols.iter().map(|(i, _)| *i).zip(scores).collect();
        let wm = tx.watermark(katalogr_store::RANK_CONTEXT).unwrap();
        tx.store_rank(&stored, wm).unwrap();
        tx.commit().unwrap();
        Bench { _tmp: tmp, cache }
    }

    /// FEAT-077 R-12: each insight on a graph with one 230-target key named by 5,000 fan rows
    /// (1.15 M expanded entries) within 2 s.
    #[test]
    #[ignore]
    fn fanout_heavy_insights() {
        use katalogr_analytics::{CoverageInput, coverage};
        let b = build_fan();
        let read = b.cache.read().unwrap();
        let links = read.links_all().unwrap();
        let entries: usize = links.iter().map(|(_, r)| r.entries.len()).sum();
        println!("expanded entries: {entries}");
        assert!(entries >= 1_150_000);
        let start = Instant::now();
        let m = katalogr_query::map(&read).unwrap();
        report("fanout_heavy_insights (map)", start, 2000);
        assert!(!m.is_empty());
        let start = Instant::now();
        let r = hubs(
            &read,
            &HubOptions {
                graph: graph(Level::Symbol, true),
                min_degree: 10.0,
                limit: 20,
            },
        )
        .unwrap();
        report("fanout_heavy_insights (hubs)", start, 2000);
        assert!(!r.items.is_empty());
        let start = Instant::now();
        cycles(
            &read,
            &CycleOptions {
                graph: graph(Level::Symbol, true),
                members: 10,
                limit: 20,
            },
        )
        .unwrap();
        report("fanout_heavy_insights (cycles)", start, 2000);
        let start = Instant::now();
        dead_code(
            &read,
            &DeadCodeOptions {
                code_kinds: BTreeMap::from([(
                    "test".to_string(),
                    BTreeSet::from(["fn".to_string()]),
                )]),
                exclude_kinds: STRUCTURAL_KINDS.iter().map(|k| k.to_string()).collect(),
                internal: false,
                min_confidence: DeadConfidence::Low,
                path_prefix: None,
                limit: 50,
            },
        )
        .unwrap();
        report("fanout_heavy_insights (dead-code)", start, 2000);
        let language = |ext: &str, _: &RelPath| ext.to_string();
        let claim = |_: &RelPath| None;
        let start = Instant::now();
        coverage(
            &read,
            &CoverageInput {
                language: &language,
                claim: &claim,
                unclaimed: BTreeMap::new(),
                roots_missing: Vec::new(),
                exclude_prefixes: Vec::new(),
                repo: None,
                suffix_top: 20,
            },
        )
        .unwrap();
        report("fanout_heavy_insights (coverage)", start, 2000);
        let symbols = read.symbols().unwrap();
        let start = Instant::now();
        katalogr_store::pagerank(&read, &symbols).unwrap();
        report("fanout_heavy_insights (pagerank)", start, 2000);
    }

    /// FEAT-076 R-12: building the insight graph over 8 million fan links adds at most 800 MiB
    /// of resident memory. 4,000 sources each name one key of 2,000 targets, so the stored rows
    /// are 4,000 fan rows and the expanded graph is 8,000,000 links.
    #[test]
    #[ignore]
    #[cfg(target_os = "linux")]
    fn insight_graph_fan_memory() {
        use katalogr_model::ExportKey;
        use katalogr_store::FanKey;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

        fn rss_kib() -> u64 {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|r| r.split_whitespace().next()?.parse().ok())
                .unwrap_or(0)
        }

        const SOURCES: usize = 4000;
        const TARGETS: usize = 2000;
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("bench").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let cache =
            Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60))
                .unwrap();
        let span = Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
        };
        let path = RelPath::new("wide.tx").unwrap();
        let provenance = Provenance {
            repo: "r".into(),
            path: path.clone(),
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        };
        let artifact = ArtifactName {
            repo: "r".into(),
            path: path.clone(),
        };
        let aid = artifact.id();
        let mut tx = cache.write().unwrap();
        let mut ids: Vec<Id> = Vec::new();
        for i in 0..SOURCES + TARGETS {
            let name = SymbolName {
                artifact: artifact.clone(),
                segments: vec![Segment {
                    kind: "fn".into(),
                    name: format!("w{i}"),
                }],
                ordinal: 1,
            };
            let id = name.id();
            tx.put_symbol(
                id,
                &SymbolRow {
                    name,
                    artifact: aid,
                    kind: "fn".into(),
                    span,
                    facts: BTreeMap::new(),
                },
            )
            .unwrap();
            ids.push(id);
        }
        let mut symbols = ids.clone();
        symbols.sort();
        tx.put_artifact(
            aid,
            &ArtifactRow {
                name: artifact,
                extension: "test".into(),
                provenance: provenance.clone(),
                facts: BTreeMap::new(),
                symbols,
                exports: Vec::new(),
                refs: Vec::new(),
            },
        )
        .unwrap();
        let key = ExportKey {
            namespace: "m".into(),
            key: "area".into(),
        };
        let mut targets: Vec<Id> = ids[..TARGETS].to_vec();
        targets.sort();
        tx.put_exports(&key, &targets).unwrap();
        for id in &ids[TARGETS..] {
            tx.put_fan(
                &FanKey {
                    from: *id,
                    kind: LinkKind("calls".into()),
                    key: key.clone(),
                },
                &LinkRow {
                    entries: vec![LinkEntry {
                        origin: LinkOrigin::Ref(0),
                        confidence: Confidence::Inferred,
                        span: Some(span),
                        provenance: provenance.clone(),
                        fanout: None,
                        produced_by: None,
                    }],
                },
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let read = cache.read().unwrap();
        assert_eq!(read.fans_all().unwrap().len(), SOURCES);
        let base = rss_kib();
        let peak = Arc::new(AtomicU64::new(base));
        let stop = Arc::new(AtomicBool::new(false));
        let sampler = {
            let (peak, stop) = (peak.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    peak.fetch_max(rss_kib(), Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        };
        let started = Instant::now();
        let r = hubs(
            &read,
            &HubOptions {
                graph: graph(Level::Symbol, true),
                min_degree: 1.0,
                limit: 20,
            },
        )
        .unwrap();
        let took = started.elapsed();
        stop.store(true, Ordering::Relaxed);
        sampler.join().unwrap();
        let grown_mib = peak.load(Ordering::Relaxed).saturating_sub(base) / 1024;
        println!(
            "load {}: hubs over {} expanded links in {took:?}, peak +{grown_mib} MiB",
            std::fs::read_to_string("/proc/loadavg")
                .map(|s| s.split_whitespace().next().unwrap_or("?").to_string())
                .unwrap_or_default(),
            SOURCES * TARGETS,
        );
        assert!(!r.items.is_empty());
        assert!(grown_mib <= 800, "+{grown_mib} MiB");
    }
}
