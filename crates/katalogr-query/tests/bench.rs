//! Timing benchmarks, run on demand: `cargo test -p katalogr-query --release -- --ignored --nocapture`.

use camino::Utf8PathBuf;
use katalogr_model::{
    ArtifactName, Confidence, Field, Fragment, Id, LinkKind, Name, Provenance, RelPath, Segment,
    Span, SymbolName, tokenize,
};
use katalogr_query::{rank, refs, resolve, result::Resolution, search};
use katalogr_store::{
    Cache, Kindex, LEXICON_FIELDS, LinkEntry, LinkKey, LinkOrigin, LinkRow, ProjectConfig,
    ProjectName, Reader, RepoConfig, Restitch, StoreError, SymbolRow, WriteTx,
};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

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

const SYMBOLS: usize = 100_000;

#[test]
#[ignore]
fn query_refs_100k() {
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
        Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60)).unwrap();

    // One artifact, SYMBOLS functions named s0..sN, and every other function calls the hub `hub`.
    let path = RelPath::new("bench.tx").unwrap();
    let artifact = ArtifactName {
        repo: "r".into(),
        path: path.clone(),
    };
    let artifact_id = artifact.id();
    let provenance = Provenance {
        repo: "r".into(),
        path: path.clone(),
        revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
    };
    let span = Span {
        start_line: 1,
        start_col: 1,
        end_line: 1,
        end_col: 1,
    };
    let symbol = |name: String| SymbolName {
        artifact: artifact.clone(),
        segments: vec![Segment {
            kind: "fn".into(),
            name,
        }],
        ordinal: 1,
    };
    let hub = symbol("hub".into());
    let hub_id = hub.id();
    let mut tx = cache.write().unwrap();
    tx.put_symbol(
        hub_id,
        &SymbolRow {
            name: hub,
            artifact: artifact_id,
            kind: "fn".into(),
            span,
            facts: Default::default(),
        },
    )
    .unwrap();
    for i in 0..SYMBOLS {
        let name = symbol(format!("s{i}"));
        let from = name.id();
        tx.put_symbol(
            from,
            &SymbolRow {
                name,
                artifact: artifact_id,
                kind: "fn".into(),
                span,
                facts: Default::default(),
            },
        )
        .unwrap();
        tx.put_link(
            &LinkKey {
                from,
                kind: LinkKind("calls".into()),
                to: hub_id,
            },
            &LinkRow {
                entries: vec![LinkEntry {
                    origin: LinkOrigin::Intra,
                    confidence: Confidence::Extracted,
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
    let Resolution::One(target) = resolve(&read, "hub").unwrap() else {
        panic!("hub must resolve to one symbol");
    };
    let start = Instant::now();
    let hits = refs(&read, &target).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(hits.len(), SYMBOLS);
    println!(
        "bench::query_refs_100k: {} ms",
        elapsed.as_secs_f64() * 1000.0
    );
}

/// The 200 words that symbol names are built from.
const WORDS: [&str; 200] = [
    "alpha",
    "bravo",
    "cargo",
    "delta",
    "echo",
    "foxtrot",
    "golf",
    "hotel",
    "india",
    "juliet",
    "kilo",
    "lima",
    "mike",
    "november",
    "oscar",
    "papa",
    "quebec",
    "romeo",
    "sierra",
    "tango",
    "uniform",
    "victor",
    "whiskey",
    "xray",
    "yankee",
    "zulu",
    "amber",
    "birch",
    "cedar",
    "dune",
    "elm",
    "fern",
    "grove",
    "heath",
    "ivy",
    "juniper",
    "kelp",
    "larch",
    "moss",
    "nettle",
    "oak",
    "pine",
    "reed",
    "sage",
    "thyme",
    "umber",
    "vine",
    "willow",
    "yarrow",
    "zinnia",
    "anchor",
    "bridge",
    "canal",
    "dock",
    "ember",
    "forge",
    "gate",
    "harbor",
    "island",
    "jetty",
    "kiln",
    "ledger",
    "market",
    "number",
    "orbit",
    "parcel",
    "quarry",
    "rail",
    "signal",
    "tunnel",
    "upland",
    "valley",
    "wharf",
    "yard",
    "zone",
    "arch",
    "beam",
    "column",
    "dome",
    "export",
    "filter",
    "gauge",
    "hinge",
    "index",
    "joint",
    "key",
    "lever",
    "mount",
    "nozzle",
    "offset",
    "pivot",
    "quota",
    "router",
    "socket",
    "token",
    "unit",
    "vector",
    "wheel",
    "axis",
    "buffer",
    "cache",
    "daemon",
    "event",
    "frame",
    "graph",
    "handle",
    "input",
    "journal",
    "kernel",
    "layer",
    "module",
    "node",
    "object",
    "parser",
    "query",
    "record",
    "schema",
    "thread",
    "update",
    "value",
    "widget",
    "yield",
    "zipper",
    "action",
    "branch",
    "client",
    "driver",
    "engine",
    "flush",
    "gather",
    "helper",
    "invoke",
    "loader",
    "mapper",
    "normal",
    "origin",
    "print",
    "reader",
    "server",
    "target",
    "render",
    "shard",
    "stream",
    "switch",
    "timer",
    "union",
    "varint",
    "writer",
    "bundle",
    "chunk",
    "commit",
    "deploy",
    "emit",
    "fetch",
    "guard",
    "hashed",
    "inline",
    "lookup",
    "merge",
    "notify",
    "option",
    "prefix",
    "reduce",
    "scope",
    "trace",
    "unwrap",
    "verify",
    "wrapper",
    "marker",
    "sorted",
    "crate",
    "dispatch",
    "extract",
    "format",
    "glossary",
    "hasher",
    "indexer",
    "joiner",
    "keyword",
    "lexer",
    "matcher",
    "nested",
    "outline",
    "planner",
    "quoter",
    "resolver",
    "sampler",
    "tracker",
    "uploader",
    "validator",
    "weight",
    "anvil",
    "bobbin",
    "cobalt",
    "dynamo",
    "fathom",
    "garnet",
    "hazel",
    "iron",
    "jasper",
];

/// Seed for every pseudo-random choice in the benchmarks, so runs are repeatable.
const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// xorshift64: a small deterministic generator; `state` must start non-zero.
fn next_rand(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// A cache in `tmp` with no shards, as the query benches use.
fn open_bench_cache(tmp: &tempfile::TempDir) -> Cache {
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
    Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60)).unwrap()
}

/// Index a symbol whose only searchable text is `name`, exactly as the engine's lexicon does for
/// a symbol with no doc, section or contract facts (`katalogr_engine::lexicon::doc_of`).
fn index_name(tx: &mut WriteTx, id: Id, name: &str) {
    let mut lens = [0u32; LEXICON_FIELDS];
    let mut terms: BTreeMap<(String, u8), u32> = BTreeMap::new();
    for t in tokenize(name) {
        lens[Field::Name.index()] += 1;
        *terms.entry((t, Field::Name.index() as u8)).or_default() += 1;
    }
    let terms: Vec<(String, u8, u32)> = terms.into_iter().map(|((t, f), n)| (t, f, n)).collect();
    tx.put_doc_postings(id, lens, &terms).unwrap();
}

#[test]
#[ignore]
fn search_100k() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = open_bench_cache(&tmp);

    let path = RelPath::new("bench.tx").unwrap();
    let artifact = ArtifactName {
        repo: "r".into(),
        path,
    };
    let artifact_id = artifact.id();
    let span = Span {
        start_line: 1,
        start_col: 1,
        end_line: 1,
        end_col: 1,
    };

    // SYMBOLS functions named fn_<word1>_<word2>_<i>, each word drawn from WORDS.
    let mut rng = SEED;
    let mut tx = cache.write().unwrap();
    for i in 0..SYMBOLS {
        let w1 = WORDS[next_rand(&mut rng) as usize % WORDS.len()];
        let w2 = WORDS[next_rand(&mut rng) as usize % WORDS.len()];
        let name = SymbolName {
            artifact: artifact.clone(),
            segments: vec![Segment {
                kind: "fn".into(),
                name: format!("fn_{w1}_{w2}_{i}"),
            }],
            ordinal: 1,
        };
        let id = name.id();
        index_name(&mut tx, id, &name.segments[0].name);
        tx.put_symbol(
            id,
            &SymbolRow {
                name,
                artifact: artifact_id,
                kind: "fn".into(),
                span,
                facts: Default::default(),
            },
        )
        .unwrap();
    }
    tx.commit().unwrap();

    let read = cache.read().unwrap();
    let start = Instant::now();
    let hits = search(&read, "ember cache", 20).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(hits.len(), 20);
    println!("bench::search_100k: {} ms", elapsed.as_secs_f64() * 1000.0);
}

#[test]
#[ignore]
fn rank_100k() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = open_bench_cache(&tmp);

    let path = RelPath::new("bench.tx").unwrap();
    let artifact = ArtifactName {
        repo: "r".into(),
        path: path.clone(),
    };
    let artifact_id = artifact.id();
    let provenance = Provenance {
        repo: "r".into(),
        path,
        revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
    };
    let span = Span {
        start_line: 1,
        start_col: 1,
        end_line: 1,
        end_col: 1,
    };

    // SYMBOLS symbols; symbol i links to three distinct pseudo-random other symbols.
    let mut tx = cache.write().unwrap();
    let mut ids: Vec<Id> = Vec::with_capacity(SYMBOLS);
    for i in 0..SYMBOLS {
        let name = SymbolName {
            artifact: artifact.clone(),
            segments: vec![Segment {
                kind: "fn".into(),
                name: format!("s{i}"),
            }],
            ordinal: 1,
        };
        let id = name.id();
        tx.put_symbol(
            id,
            &SymbolRow {
                name,
                artifact: artifact_id,
                kind: "fn".into(),
                span,
                facts: Default::default(),
            },
        )
        .unwrap();
        ids.push(id);
    }
    let mut rng = SEED;
    for (i, from) in ids.iter().enumerate() {
        let mut targets: Vec<usize> = Vec::with_capacity(3);
        while targets.len() < 3 {
            let j = next_rand(&mut rng) as usize % SYMBOLS;
            if j != i && !targets.contains(&j) {
                targets.push(j);
            }
        }
        for j in targets {
            tx.put_link(
                &LinkKey {
                    from: *from,
                    kind: LinkKind("calls".into()),
                    to: ids[j],
                },
                &LinkRow {
                    entries: vec![LinkEntry {
                        origin: LinkOrigin::Intra,
                        confidence: Confidence::Extracted,
                        span: Some(span),
                        provenance: provenance.clone(),
                        fanout: None,
                        produced_by: None,
                    }],
                },
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();

    let read = cache.read().unwrap();
    let start = Instant::now();
    let ranked = rank(&read).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(ranked.len(), SYMBOLS);
    println!("bench::rank_100k: {} ms", elapsed.as_secs_f64() * 1000.0);
}

#[test]
#[ignore]
fn session_start_100k() {
    // The same graph as `rank_100k`, with the rank stored as the processor stores it at publish.
    let tmp = tempfile::tempdir().unwrap();
    let cache = open_bench_cache(&tmp);
    let path = RelPath::new("bench.tx").unwrap();
    let artifact = ArtifactName {
        repo: "r".into(),
        path: path.clone(),
    };
    let artifact_id = artifact.id();
    let provenance = Provenance {
        repo: "r".into(),
        path,
        revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
    };
    let span = Span {
        start_line: 1,
        start_col: 1,
        end_line: 1,
        end_col: 1,
    };
    let mut tx = cache.write().unwrap();
    let mut ids: Vec<Id> = Vec::with_capacity(SYMBOLS);
    for i in 0..SYMBOLS {
        let name = SymbolName {
            artifact: artifact.clone(),
            segments: vec![Segment {
                kind: "fn".into(),
                name: format!("s{i}"),
            }],
            ordinal: 1,
        };
        let id = name.id();
        tx.put_symbol(
            id,
            &SymbolRow {
                name,
                artifact: artifact_id,
                kind: "fn".into(),
                span,
                facts: Default::default(),
            },
        )
        .unwrap();
        ids.push(id);
    }
    let mut rng = SEED;
    for (i, from) in ids.iter().enumerate() {
        let mut targets: Vec<usize> = Vec::with_capacity(3);
        while targets.len() < 3 {
            let j = next_rand(&mut rng) as usize % SYMBOLS;
            if j != i && !targets.contains(&j) {
                targets.push(j);
            }
        }
        for j in targets {
            tx.put_link(
                &LinkKey {
                    from: *from,
                    kind: LinkKind("calls".into()),
                    to: ids[j],
                },
                &LinkRow {
                    entries: vec![LinkEntry {
                        origin: LinkOrigin::Intra,
                        confidence: Confidence::Extracted,
                        span: Some(span),
                        provenance: provenance.clone(),
                        fanout: None,
                        produced_by: None,
                    }],
                },
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();

    // The publish step: compute once and store.
    let mut tx = cache.write().unwrap();
    let symbols = tx.symbols().unwrap();
    let scores = katalogr_store::pagerank(&tx, &symbols).unwrap();
    let stored: Vec<(Id, f64)> = symbols.iter().map(|(i, _)| *i).zip(scores).collect();
    let wm = tx.watermark(katalogr_store::RANK_CONTEXT).unwrap();
    tx.store_rank(&stored, wm).unwrap();
    tx.commit().unwrap();

    // Session start: read the rank from the snapshot and take the top symbols.
    let start = Instant::now();
    let read = cache.read().unwrap();
    let ranked = rank(&read).unwrap();
    let top: Vec<_> = ranked.iter().take(10).collect();
    let elapsed = start.elapsed();
    assert_eq!(ranked.len(), SYMBOLS);
    assert_eq!(top.len(), 10);
    let live = katalogr_query::rank_live(&read).unwrap();
    assert_eq!(ranked, live, "the stored rank equals the live rank");
    println!(
        "bench::session_start_100k: {} ms",
        elapsed.as_secs_f64() * 1000.0
    );
    assert!(elapsed < Duration::from_millis(1000), "over budget");
}
