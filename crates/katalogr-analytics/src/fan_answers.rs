//! FEAT-077 R-3: a graph stored with fan rows answers every question exactly as the same graph
//! stored with one link row per target.

use crate::options::{
    CommunityOptions, CycleOptions, DEFAULT_SEED, DeadCodeOptions, GraphOptions, HubOptions,
};
use crate::result::{DeadConfidence, Level};
use crate::testkit::{Built, Fx};
use crate::{CoverageInput, communities, coverage, cycles, dead_code, hubs};
use katalogr_model::{Id, RelPath};
use katalogr_query::{
    SymbolHit, TestsOptions, callers, explain, map, path, rank_live, refs, tests,
};
use katalogr_store::{Reader, pagerank};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// 3 files, 12 symbols. `m:area` has 3 targets, `m:new` has 5. One stored row overlaps a fan
/// target, one fan row reaches its own source, a `verified-by` link, and a cycle through a fan.
fn graph(fan_rows: bool) -> Built {
    let mut fx = Fx::new();
    let a: Vec<Id> = (1..=4).map(|i| fx.sym("a.tx", &format!("a{i}"))).collect();
    let b: Vec<Id> = (1..=4).map(|i| fx.sym("b.tx", &format!("b{i}"))).collect();
    let c: Vec<Id> = (1..=4).map(|i| fx.sym("c.tx", &format!("c{i}"))).collect();
    let area = [a[0], b[0], c[0]];
    let new = [a[1], b[1], c[1], a[2], b[2]];
    let put = |fx: &mut Fx, from: Id, key: &str, idx: u32, ts: &[Id]| {
        if fan_rows {
            fx.fan(from, "calls", ("m", key), idx, ts);
        } else {
            fx.fan_flat(from, "calls", ("m", key), idx, ts);
        }
    };
    put(&mut fx, a[3], "area", 0, &area);
    put(&mut fx, a[1], "area", 0, &area);
    put(&mut fx, c[0], "area", 0, &area); // reaches itself
    put(&mut fx, b[3], "new", 0, &new);
    put(&mut fx, c[3], "new", 1, &new);
    fx.link(a[3], "calls", b[0]); // overlaps a fan target
    fx.link(b[0], "calls", a[3]); // closes a cycle through the fan edge
    fx.link(c[3], "calls", c[2]);
    fx.link(a[0], "contains", a[1]);
    let t = fx.sym("tests/t.tx", "t_area");
    fx.link(t, "calls", a[3]);
    fx.link(a[0], "verified-by", t);
    fx.link(t, "verifies", a[0]);
    fx.build()
}

fn hit_of(tx: &impl Reader, id: Id) -> SymbolHit {
    let row = tx.symbol(id).unwrap().unwrap();
    SymbolHit {
        name: row.name.canonical(),
        kind: row.kind.clone(),
        path: row.name.artifact.path.as_str().to_string(),
        span: row.span,
    }
}

fn answers(built: &Built) -> BTreeMap<String, Value> {
    let tx = built.read();
    let mut out = BTreeMap::new();
    let mut put = |k: &str, v: Value| {
        out.insert(k.to_string(), v);
    };
    put("links_all", json!(format!("{:?}", tx.links_all().unwrap())));
    put("map", serde_json::to_value(map(&tx).unwrap()).unwrap());
    let ids: Vec<Id> = tx.symbols().unwrap().into_iter().map(|(i, _)| i).collect();
    for id in &ids {
        let h = hit_of(&tx, *id);
        let n = h.name.clone();
        put(
            &format!("refs {n}"),
            serde_json::to_value(refs(&tx, &h).unwrap()).unwrap(),
        );
        put(
            &format!("callers {n}"),
            serde_json::to_value(callers(&tx, &h, 2).unwrap()).unwrap(),
        );
        put(
            &format!("tests {n}"),
            serde_json::to_value(tests(&tx, &h, TestsOptions::default()).unwrap()).unwrap(),
        );
        put(
            &format!("explain {n}"),
            serde_json::to_value(explain(&tx, &h, 50).unwrap()).unwrap(),
        );
    }
    let first = hit_of(&tx, ids[0]);
    for id in &ids {
        let h = hit_of(&tx, *id);
        put(
            &format!("path {} {}", first.name, h.name),
            serde_json::to_value(path(&tx, &first, &h, 6).unwrap()).unwrap(),
        );
    }
    for inferred in [false, true] {
        let graph = GraphOptions::new(Level::Symbol, inferred);
        put(
            &format!("hubs {inferred}"),
            serde_json::to_value(
                hubs(
                    &tx,
                    &HubOptions {
                        graph: graph.clone(),
                        min_degree: 0.0,
                        limit: 100,
                    },
                )
                .unwrap(),
            )
            .unwrap(),
        );
        put(
            &format!("cycles {inferred}"),
            serde_json::to_value(
                cycles(
                    &tx,
                    &CycleOptions {
                        graph: graph.clone(),
                        members: 50,
                        limit: 50,
                    },
                )
                .unwrap(),
            )
            .unwrap(),
        );
        put(
            &format!("communities {inferred}"),
            serde_json::to_value(
                communities(
                    &tx,
                    &CommunityOptions {
                        graph,
                        seed: DEFAULT_SEED,
                        resolution: 1.0,
                        min_size: 1,
                        members: 50,
                        limit: 50,
                        of: None,
                    },
                )
                .unwrap(),
            )
            .unwrap(),
        );
    }
    put(
        "dead_code",
        serde_json::to_value(
            dead_code(
                &tx,
                &DeadCodeOptions {
                    code_kinds: BTreeMap::from([(
                        "test".to_string(),
                        BTreeSet::from(["fn".to_string()]),
                    )]),
                    exclude_kinds: crate::options::STRUCTURAL_KINDS
                        .iter()
                        .map(|k| k.to_string())
                        .collect(),
                    internal: false,
                    min_confidence: DeadConfidence::Low,
                    path_prefix: None,
                    limit: 100,
                },
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let language = |_: &str, _: &RelPath| "t".to_string();
    let claim = |_: &RelPath| None;
    put(
        "coverage",
        serde_json::to_value(
            coverage(
                &tx,
                &CoverageInput {
                    language: &language,
                    claim: &claim,
                    unclaimed: BTreeMap::new(),
                    roots_missing: Vec::new(),
                    exclude_prefixes: Vec::new(),
                    repo: None,
                    suffix_top: 5,
                },
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let symbols = tx.symbols().unwrap();
    let bits: Vec<u64> = pagerank(&tx, &symbols)
        .unwrap()
        .iter()
        .map(|f| f.to_bits())
        .collect();
    put("pagerank", json!(bits));
    put(
        "rank_live",
        json!(
            rank_live(&tx)
                .unwrap()
                .iter()
                .map(|(h, f)| (h.name.clone(), f.to_bits()))
                .collect::<Vec<_>>()
        ),
    );
    out
}

#[test]
fn representations_answer_alike() {
    let (fan, flat) = (answers(&graph(true)), answers(&graph(false)));
    assert_eq!(
        fan.keys().collect::<Vec<_>>(),
        flat.keys().collect::<Vec<_>>()
    );
    for (k, v) in &fan {
        assert_eq!(v, &flat[k], "answer {k}");
    }
    // The fan graph really stores fan rows, and the flat one none.
    assert_eq!(graph(true).read().fans_all().unwrap().len(), 5);
    assert!(graph(false).read().fans_all().unwrap().is_empty());
}
