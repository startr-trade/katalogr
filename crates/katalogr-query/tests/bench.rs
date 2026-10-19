//! Timing benchmarks, run on demand: `cargo test -p katalogr-query --release -- --ignored --nocapture`.

use camino::Utf8PathBuf;
use katalogr_model::{
    ArtifactName, Confidence, Fragment, LinkKind, Name, Provenance, RelPath, Segment, Span,
    SymbolName,
};
use katalogr_query::{refs, resolve, result::Resolution};
use katalogr_store::{
    Cache, Kindex, LinkEntry, LinkKey, LinkOrigin, LinkRow, ProjectConfig, ProjectName, RepoConfig,
    Restitch, StoreError, SymbolRow, WriteTx,
};
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
