//! Dependency cycles: the strongly connected components of the insight graph, each with a
//! witness (FEAT-030 section 6.3).

use crate::graph::InsightGraph;
use crate::options::CycleOptions;
use crate::result::{Cycle, CycleReport, NodeHit};
use crate::scc::{tarjan, witness};
use katalogr_model::Confidence;
use katalogr_query::result::{Direction, PathStep};
use katalogr_store::{Reader, StoreError};

/// Strongly connected components of size two or more, largest first, with a witness cycle
/// each. The report's `confidence` is the weakest on the witness, so a cycle built only from
/// extracted links is `extracted`.
pub fn cycles(tx: &impl Reader, opts: &CycleOptions) -> Result<CycleReport, StoreError> {
    let g = InsightGraph::load(tx, &opts.graph)?;
    let name = |v: u32| g.nodes[v as usize].name.as_str();
    let mut comps = tarjan(&g);
    let cyclic_nodes: u64 = comps.iter().map(|c| c.len() as u64).sum();
    let smallest = |c: &[u32]| -> u32 {
        *c.iter()
            .min_by(|a, b| name(**a).cmp(name(**b)))
            .expect("a component has nodes")
    };
    comps.sort_by(|a, b| {
        b.len()
            .cmp(&a.len())
            .then_with(|| name(smallest(a)).cmp(name(smallest(b))))
    });
    let cycles_total = comps.len() as u64;
    let items = comps
        .iter()
        .take(opts.limit)
        .enumerate()
        .map(|(i, comp)| {
            let mut members: Vec<u32> = comp.clone();
            members.sort_by(|a, b| name(*a).cmp(name(*b)));
            members.truncate(opts.members);
            let steps = witness(&g, comp);
            let confidence = if steps.iter().any(|s| s.3 == crate::graph::Strength::Weak) {
                Confidence::Inferred
            } else {
                Confidence::Extracted
            };
            Cycle {
                id: i as u32 + 1,
                size: comp.len() as u64,
                members: members
                    .iter()
                    .map(|v| g.nodes[*v as usize].clone())
                    .collect::<Vec<NodeHit>>(),
                members_total: comp.len() as u64,
                witness: steps
                    .iter()
                    .map(|&(from, kind, to, strength)| PathStep {
                        from: name(from).to_string(),
                        kind: g.kinds[kind as usize].clone(),
                        direction: Direction::Forward,
                        to: name(to).to_string(),
                        confidence: strength.confidence(),
                    })
                    .collect(),
                confidence,
            }
        })
        .collect();
    Ok(CycleReport {
        level: opts.graph.level,
        include_inferred: opts.graph.include_inferred,
        cycles_total,
        cyclic_nodes,
        skipped_contracts: g.skipped_contracts,
        items,
    })
}
