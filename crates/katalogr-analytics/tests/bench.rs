//! Timing benchmarks, run on demand:
//! `cargo test -p katalogr-analytics --release --test bench -- --ignored --nocapture --test-threads=1`.
//!
//! The graph has 100,000 `fn` symbols in 1,000 files of 20 packages and 300,000 links; the rank
//! is stored as the processor stores it at publish.

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
                if i % 10 == 0 {
                    facts.insert("root".to_string(), Fact::Str("main".into()));
                }
                if i % 100 == 0 {
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
                let inferred = count % 50 == 0;
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
}
