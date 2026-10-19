//! `rank`: PageRank over the reference graph (FEAT-025 section 6), used by `map`.

use crate::resolve::hit;
use crate::result::SymbolHit;
use katalogr_model::Id;
use katalogr_store::{LinkEntry, Reader, StoreError};
use std::collections::BTreeMap;

const DAMPING: f64 = 0.85;
const ITERATIONS: usize = 30;

/// The weight of one support entry of a link. Every link counts the same today; this is the
/// one place to change when links are to be weighted (for example by confidence).
pub fn link_weight(_entry: &LinkEntry) -> f64 {
    1.0
}

/// PageRank of every symbol: damping 0.85, 30 iterations, all link kinds, uniform teleport,
/// the mass of symbols with no outgoing link spread evenly. A link `a -> b` passes rank from `a`
/// to `b`. A link row counts once, with the largest [`link_weight`] of its entries. Links
/// whose ends are not both symbols, and links from a symbol to itself, are ignored. Sorted by
/// rank descending, then name.
pub fn rank(tx: &impl Reader) -> Result<Vec<(SymbolHit, f64)>, StoreError> {
    let symbols = tx.symbols()?;
    let n = symbols.len();
    if n == 0 {
        return Ok(Vec::new());
    }
    let index: BTreeMap<Id, usize> = symbols
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i))
        .collect();
    let mut edges: Vec<(usize, usize, f64)> = Vec::new();
    let mut out_weight = vec![0.0f64; n];
    for (key, row) in tx.links_all()? {
        let (Some(&a), Some(&b)) = (index.get(&key.from), index.get(&key.to)) else {
            continue;
        };
        if a == b {
            continue;
        }
        let w = row.entries.iter().map(link_weight).fold(0.0f64, f64::max);
        if w <= 0.0 {
            continue;
        }
        out_weight[a] += w;
        edges.push((a, b, w));
    }
    let nf = n as f64;
    let mut r = vec![1.0 / nf; n];
    for _ in 0..ITERATIONS {
        let dangling: f64 = (0..n).filter(|i| out_weight[*i] == 0.0).map(|i| r[i]).sum();
        let base = (1.0 - DAMPING) / nf + DAMPING * dangling / nf;
        let mut next = vec![base; n];
        for &(a, b, w) in &edges {
            next[b] += DAMPING * r[a] * w / out_weight[a];
        }
        r = next;
    }
    let mut out: Vec<(SymbolHit, f64)> = symbols
        .iter()
        .zip(r)
        .map(|((_, row), score)| (hit(row), score))
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.name.cmp(&b.0.name)));
    Ok(out)
}
