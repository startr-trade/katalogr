//! Structure: the most important nodes of the insight graph and the edges between them, for
//! the diagram views (FEAT-032).

use crate::graph::{InsightGraph, Strength};
use crate::options::GraphOptions;
use crate::result::NodeHit;
use katalogr_store::{Reader, StoreError};
use std::collections::BTreeSet;

/// A node that was kept, with its rank (summed over its symbols above symbol level).
#[derive(Clone, Debug, PartialEq)]
pub struct GraphNode {
    pub node: NodeHit,
    pub rank: f64,
}

/// An edge between two kept nodes, naming the ends by node name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
    /// The edge is weak: inferred, or its reference resolved to several targets.
    pub inferred: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructureReport {
    /// Ordered by rank, highest first, then name.
    pub nodes: Vec<GraphNode>,
    /// Between kept nodes only, sorted by `from`, `to`, `kind`.
    pub edges: Vec<GraphEdge>,
    /// Nodes in the whole graph; more than `nodes.len()` means the report is partial.
    pub nodes_total: u64,
}

/// The top `max_nodes` nodes of the graph at `opts.level` by rank, then name, and the edges
/// with both ends kept.
pub fn structure(
    tx: &(impl Reader + Sync),
    opts: &GraphOptions,
    max_nodes: usize,
) -> Result<StructureReport, StoreError> {
    let g = InsightGraph::load(tx, opts)?;
    let n = g.len();
    let nodes_total = n as u64;
    if n == 0 || max_nodes == 0 {
        return Ok(StructureReport {
            nodes: Vec::new(),
            edges: Vec::new(),
            nodes_total,
        });
    }
    let ranks = g.node_ranks(tx)?;
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.sort_by(|a, b| {
        ranks[*b as usize]
            .total_cmp(&ranks[*a as usize])
            .then_with(|| g.nodes[*a as usize].name.cmp(&g.nodes[*b as usize].name))
    });
    order.truncate(max_nodes);
    let kept: BTreeSet<u32> = order.iter().copied().collect();
    let mut edges: Vec<GraphEdge> = Vec::new();
    for &v in &order {
        for e in g.out_edges(v) {
            if kept.contains(&e.to) {
                edges.push(GraphEdge {
                    from: g.nodes[v as usize].name.clone(),
                    to: g.nodes[e.to as usize].name.clone(),
                    kind: g.kinds[e.kind as usize].clone(),
                    inferred: e.strength == Strength::Weak,
                });
            }
        }
    }
    edges.sort();
    edges.dedup();
    let nodes = order
        .iter()
        .map(|&v| GraphNode {
            node: g.nodes[v as usize].clone(),
            rank: ranks[v as usize],
        })
        .collect();
    Ok(StructureReport {
        nodes,
        edges,
        nodes_total,
    })
}

/// R-2 (FEAT-032): the cap keeps the nodes with the highest rank, ties go to the lower name,
/// and only edges with both ends kept remain, sorted.
#[cfg(test)]
#[test]
fn caps_by_rank_then_name() {
    use crate::result::Level;
    use crate::testkit::Fx;

    let mut fx = Fx::new();
    let hub = fx.sym("a.tx", "hub");
    let x = fx.sym("a.tx", "x");
    let y = fx.sym("a.tx", "y");
    let z = fx.sym("a.tx", "z");
    let far = fx.sym("a.tx", "far");
    fx.link(x, "calls", hub);
    fx.link(y, "calls", hub);
    fx.link(z, "calls", hub);
    fx.link(hub, "calls", far);
    let built = fx.build();
    let opts = GraphOptions::new(Level::Symbol, false);

    let all = structure(&built.read(), &opts, 100).unwrap();
    assert_eq!(all.nodes_total, 5);
    assert_eq!(all.nodes.len(), 5);
    assert_eq!(all.edges.len(), 4);
    assert!(all.edges.windows(2).all(|w| w[0] <= w[1]), "edges sorted");
    assert!(all.edges.iter().all(|e| !e.inferred && e.kind == "calls"));
    for w in all.nodes.windows(2) {
        assert!(
            w[0].rank > w[1].rank || (w[0].rank == w[1].rank && w[0].node.name < w[1].node.name),
            "rank descending, then name: {:?} {:?}",
            w[0],
            w[1]
        );
    }
    // `far` is the end of the chain, so it collects the most rank; `hub` follows it.
    assert!(all.nodes[0].node.name.ends_with("far"));
    assert!(all.nodes[1].node.name.ends_with("hub"));

    let top = structure(&built.read(), &opts, 1).unwrap();
    assert_eq!(top.nodes_total, 5);
    assert_eq!(top.nodes.len(), 1);
    assert!(top.nodes[0].node.name.ends_with("far"));
    assert!(top.edges.is_empty(), "no edge has both ends kept");
    let two = structure(&built.read(), &opts, 2).unwrap();
    assert_eq!(two.edges.len(), 1, "only hub to far has both ends kept");

    // The kept set is the prefix of the full order, and edges stay inside it.
    let three = structure(&built.read(), &opts, 3).unwrap();
    assert_eq!(three.nodes[..], all.nodes[..3]);
    let kept: Vec<&str> = three.nodes.iter().map(|n| n.node.name.as_str()).collect();
    assert!(
        three
            .edges
            .iter()
            .all(|e| kept.contains(&e.from.as_str()) && kept.contains(&e.to.as_str()))
    );

    assert!(structure(&built.read(), &opts, 0).unwrap().nodes.is_empty());
}
