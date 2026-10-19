//! PageRank over the reference graph (FEAT-025 section 6, FEAT-057). It lives in the store so
//! the engine can compute and cache it at publish while the query crate serves it.

use crate::cache::{LinkEntry, LinkRow, Reader, SymbolRow};
use crate::error::StoreError;
use katalogr_model::{Confidence, Id};
use std::collections::BTreeMap;

/// The PageRank damping factor.
pub const PAGERANK_DAMPING: f64 = 0.85;
/// The number of PageRank iterations.
pub const PAGERANK_ITERATIONS: u32 = 30;

/// The context whose watermark the stored rank is compared against.
pub const RANK_CONTEXT: &str = "worktree";

/// The weight of one support entry of a link: extracted and declared entries weigh 1; an
/// inferred entry weighs `1 / fanout`, the share of the targets its reference resolved to
/// (1 when the fanout was not recorded).
pub fn link_weight(entry: &LinkEntry) -> f64 {
    match (entry.confidence, entry.fanout) {
        (Confidence::Inferred, Some(n)) if n > 1 => 1.0 / f64::from(n),
        _ => 1.0,
    }
}

/// The weight of a link row: the largest [`link_weight`] of its entries.
pub fn row_weight(row: &LinkRow) -> f64 {
    row.entries.iter().map(link_weight).fold(0.0f64, f64::max)
}

/// PageRank of `symbols` (as `Reader::symbols` returns them), in the same order: damping 0.85,
/// 30 iterations, all link kinds, uniform teleport, the mass of symbols with no outgoing link
/// spread evenly. A link `a -> b` passes rank from `a` to `b`. A link row counts once, with its
/// [`row_weight`]. Links whose ends are not both symbols, and links from a symbol to itself,
/// are ignored.
pub fn pagerank(tx: &impl Reader, symbols: &[(Id, SymbolRow)]) -> Result<Vec<f64>, StoreError> {
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
        let w = row_weight(&row);
        if w <= 0.0 {
            continue;
        }
        out_weight[a] += w;
        edges.push((a, b, w));
    }
    let nf = n as f64;
    let mut r = vec![1.0 / nf; n];
    for _ in 0..PAGERANK_ITERATIONS {
        let dangling: f64 = (0..n).filter(|i| out_weight[*i] == 0.0).map(|i| r[i]).sum();
        let base = (1.0 - PAGERANK_DAMPING) / nf + PAGERANK_DAMPING * dangling / nf;
        let mut next = vec![base; n];
        for &(a, b, w) in &edges {
            next[b] += PAGERANK_DAMPING * r[a] * w / out_weight[a];
        }
        r = next;
    }
    Ok(r)
}
