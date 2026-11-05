//! FEAT-076 R-5: the insights that walk the links without expanding them answer exactly as the
//! same code fed by the expanding `links_all` scan did.

use crate::coverage::{classes_from, link_classes_for_test};
use crate::dead::dead_code_from;
use crate::graph::{InsightGraph, LinkSink, Strength, row_strength};
use crate::options::{DeadCodeOptions, GraphOptions, STRUCTURAL_KINDS};
use crate::result::{DeadConfidence, Level};
use crate::testkit::{Built, Fx};
use katalogr_model::insight::FACT_ROOT;
use katalogr_model::Id;
use katalogr_store::{Reader, StoreError, row_weight};
use std::collections::{BTreeMap, BTreeSet};

/// Three packages over four files; fan rows over overlapping keys, a stored link that overlaps
/// a fan target, weak and strong links, a contract with consumers and providers, a root.
fn fixture() -> Built {
    let mut fx = Fx::new();
    fx.file("a.tx", Some("p1"));
    fx.file("b.tx", Some("p1"));
    fx.file("c.tx", Some("p2"));
    fx.file("d.tx", None);
    let a: Vec<Id> = (1..=4).map(|i| fx.sym("a.tx", &format!("a{i}"))).collect();
    let b: Vec<Id> = (1..=4).map(|i| fx.sym("b.tx", &format!("b{i}"))).collect();
    let c: Vec<Id> = (1..=3).map(|i| fx.sym("c.tx", &format!("c{i}"))).collect();
    let d = fx.sym("d.tx", "d1");
    fx.sym_str(a[0], FACT_ROOT, "main");
    let area = [a[0], b[0], c[0]];
    let new = [a[1], b[1], c[1], a[2], b[2]];
    fx.fan(a[3], "calls", ("m", "area"), 0, &area);
    fx.fan(a[3], "calls", ("m", "new"), 1, &new);
    fx.fan(b[3], "uses", ("m", "area"), 0, &area);
    fx.fan(c[2], "calls", ("m", "new"), 0, &new);
    fx.link(a[3], "calls", b[0]); // overlaps a fan target
    fx.link(b[0], "calls", a[3]);
    fx.link(c[0], "calls", c[1]);
    fx.link_ref(c[1], "calls", d, 0, Some(1));
    fx.link_with(b[1], "calls", c[1], katalogr_model::Confidence::Inferred, None);
    fx.link(a[0], "contains", a[1]);
    fx.link(b[2], "calls", b[1]);
    fx.bind(a[1], false, "rpc", "svc/get");
    fx.bind(b[1], true, "rpc", "svc/get");
    fx.bind(c[1], true, "rpc", "svc/get");
    fx.build()
}

/// The old data source: every link row expanded, with the row's weight and strength.
fn expanding(
    tx: &impl Reader,
) -> impl FnMut(&mut LinkSink<'_>) -> Result<(), StoreError> + '_ {
    move |f| {
        for (key, row) in tx.links_all()? {
            f(
                key.from,
                &key.kind.0,
                key.to,
                row_weight(&row),
                row_strength(&row) == Strength::Strong,
            );
        }
        Ok(())
    }
}

fn old_kinds(tx: &impl Reader) -> BTreeSet<String> {
    tx.links_all()
        .unwrap()
        .into_iter()
        .map(|(k, _)| k.kind.0)
        .collect()
}

fn load(tx: &impl Reader, opts: &GraphOptions, old: bool) -> InsightGraph {
    let symbols = tx.symbol_cores().unwrap();
    let artifacts = tx.artifacts().unwrap();
    let contracts = tx.contracts().unwrap();
    if old {
        InsightGraph::build(symbols, artifacts, contracts, old_kinds(tx), opts, &mut expanding(tx))
    } else {
        InsightGraph::build(
            symbols,
            artifacts,
            contracts,
            tx.link_kinds().unwrap(),
            opts,
            &mut |f| tx.for_each_link(f),
        )
    }
    .unwrap()
}

#[test]
fn insight_graph_identical() {
    let built = fixture();
    let tx = built.read();
    let mut edges = 0;
    for level in [Level::Symbol, Level::File, Level::Package] {
        for inferred in [false, true] {
            let opts = GraphOptions::new(level, inferred);
            let (old, new) = (load(&tx, &opts, true), load(&tx, &opts, false));
            assert_eq!(old.nodes, new.nodes);
            assert_eq!(old.kinds, new.kinds);
            assert_eq!(old.members, new.members);
            assert_eq!(old.file_count, new.file_count);
            assert_eq!(old.file_of, new.file_of);
            assert_eq!(old.skipped_contracts, new.skipped_contracts);
            for v in 0..old.len() as u32 {
                assert_eq!(old.out_edges(v), new.out_edges(v), "{level:?} out {v}");
                assert_eq!(old.in_edges(v), new.in_edges(v), "{level:?} in {v}");
                edges += old.out_edges(v).len();
            }
        }
    }
    assert!(edges > 20, "the fixture has edges: {edges}");
    // The public path (side-by-side reads, then the walk) agrees with both.
    let opts = GraphOptions::new(Level::Symbol, true);
    let public = InsightGraph::load(&tx, &opts).unwrap();
    let old = load(&tx, &opts, true);
    for v in 0..old.len() as u32 {
        assert_eq!(old.out_edges(v), public.out_edges(v));
    }
}

#[test]
fn dead_code_identical() {
    let built = fixture();
    let tx = built.read();
    let opts = DeadCodeOptions {
        code_kinds: BTreeMap::from([("test".to_string(), BTreeSet::from(["fn".to_string()]))]),
        exclude_kinds: STRUCTURAL_KINDS.iter().map(|k| k.to_string()).collect(),
        internal: false,
        min_confidence: DeadConfidence::Low,
        path_prefix: None,
        limit: 100,
    };
    let run = |old: bool| {
        let (symbols, artifacts) = (tx.symbols().unwrap(), tx.artifacts().unwrap());
        let report = if old {
            dead_code_from(symbols, artifacts, &opts, &mut expanding(&tx))
        } else {
            dead_code_from(symbols, artifacts, &opts, &mut |f| tx.for_each_link(f))
        };
        serde_json::to_value(report.unwrap()).unwrap()
    };
    let (old, new) = (run(true), run(false));
    assert_eq!(old, new);
    assert!(old["candidates_total"].as_u64().unwrap() > 0);
    assert_eq!(serde_json::to_value(crate::dead_code(&tx, &opts).unwrap()).unwrap(), old);
}

#[test]
fn coverage_classes_identical() {
    let built = fixture();
    let tx = built.read();
    let old = link_classes_for_test(&tx);
    let new = classes_from(&mut |f| tx.for_each_ref_entry(f)).unwrap();
    assert!(!new.is_empty());
    assert_eq!(old.len(), new.len());
    for (k, v) in &old {
        assert!(new.get(k) == Some(v), "class of {k:?}");
    }
}
