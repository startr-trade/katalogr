//! PageRank over the reference graph (FEAT-025 section 6, FEAT-057). It lives in the store so
//! the engine can compute and cache it at publish while the query crate serves it.

use crate::cache::{LinkEntry, LinkRow, Reader, SymbolRow};
use crate::error::StoreError;
use katalogr_model::{Confidence, ExportKey, Id};
use std::collections::{BTreeMap, HashMap};

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

/// The rank weight of a fan row expanded over `n` targets: what [`row_weight`] gives the
/// expanded row, whose entries all carry `fanout = n`.
pub fn fan_weight(row: &LinkRow, n: usize) -> f64 {
    row.entries
        .iter()
        .map(|e| match e.confidence {
            Confidence::Inferred if n > 1 => 1.0 / n as f64,
            _ => 1.0,
        })
        .fold(0.0f64, f64::max)
}

/// The rank edges `(from index, to index, weight)` of the graph in `links_out` key order,
/// without expanding fan rows into link rows, and each source's total weight added to
/// `out_weight`. Rows whose ends are not both in `index`, self links and rows of no weight are
/// left out. A link stored and also reached by a fan row, or by two, counts once with its
/// largest weight, as the expanded graph gives it (ADR-0033).
pub(crate) fn rank_edges(
    tx: &impl Reader,
    index: &BTreeMap<Id, usize>,
    out_weight: &mut [f64],
) -> Result<Vec<(u32, u32, f64)>, StoreError> {
    // (from, kind ordinal in first-seen order, to, weight); 24 bytes each.
    let mut raw: Vec<(u32, u32, u32, f64)> = Vec::new();
    let (mut kinds, stored) = tx.stored_briefs()?;
    let mut ordinal: HashMap<String, u32> = kinds
        .iter()
        .enumerate()
        .map(|(i, k)| (k.clone(), i as u32))
        .collect();
    let mut intern = |kind: &str| -> u32 {
        if let Some(o) = ordinal.get(kind) {
            return *o;
        }
        let o = kinds.len() as u32;
        kinds.push(kind.to_string());
        ordinal.insert(kind.to_string(), o);
        o
    };
    let sym = |id: &Id| index.get(id).map(|i| *i as u32);
    for (from, ki, to, w, _) in stored {
        let (Some(a), Some(b)) = (sym(&from), sym(&to)) else {
            continue;
        };
        if a == b {
            continue;
        }
        raw.push((a, ki, b, w));
    }
    let mut targets: HashMap<ExportKey, (usize, Vec<u32>)> = HashMap::new();
    for (fk, row) in tx.fans_all()? {
        let Some(a) = sym(&fk.from) else { continue };
        if !targets.contains_key(&fk.key) {
            let all = tx.exports(&fk.key)?;
            let syms = all.iter().filter_map(sym).collect();
            targets.insert(fk.key.clone(), (all.len(), syms));
        }
        let (n, syms) = &targets[&fk.key];
        let w = fan_weight(&row, *n);
        let o = intern(&fk.kind.0);
        for &b in syms {
            if b != a {
                raw.push((a, o, b, w));
            }
        }
    }
    drop(targets);
    // Kind ordinals in `links_out` key order: the order of the length-prefixed kind bytes.
    let mut order: Vec<usize> = (0..kinds.len()).collect();
    order.sort_by(|a, b| {
        kinds[*a]
            .len()
            .cmp(&kinds[*b].len())
            .then_with(|| kinds[*a].cmp(&kinds[*b]))
    });
    let mut rank_of = vec![0u32; kinds.len()];
    for (pos, i) in order.into_iter().enumerate() {
        rank_of[i] = pos as u32;
    }
    for e in &mut raw {
        e.1 = rank_of[e.1 as usize];
    }
    raw.sort_unstable_by_key(|e| (e.0, e.1, e.2));
    let mut edges: Vec<(u32, u32, f64)> = Vec::with_capacity(raw.len());
    let mut last: Option<(u32, u32, u32)> = None;
    for (a, o, b, w) in raw {
        if last == Some((a, o, b)) {
            // The same link reached again: keep the larger weight. Its earlier weight was
            // already added to `out_weight`, so the sum is settled below.
            let prev = edges.last_mut().expect("an edge precedes a repeat");
            if w > prev.2 {
                prev.2 = w;
            }
            continue;
        }
        last = Some((a, o, b));
        edges.push((a, b, w));
    }
    edges.retain(|e| e.2 > 0.0);
    for &(a, _, w) in &edges {
        out_weight[a as usize] += w;
    }
    Ok(edges)
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
    let mut out_weight = vec![0.0f64; n];
    let edges = rank_edges(tx, &index, &mut out_weight)?;
    let nf = n as f64;
    let mut r = vec![1.0 / nf; n];
    for _ in 0..PAGERANK_ITERATIONS {
        let dangling: f64 = (0..n).filter(|i| out_weight[*i] == 0.0).map(|i| r[i]).sum();
        let base = (1.0 - PAGERANK_DAMPING) / nf + PAGERANK_DAMPING * dangling / nf;
        let mut next = vec![base; n];
        for &(a, b, w) in &edges {
            let (a, b) = (a as usize, b as usize);
            next[b] += PAGERANK_DAMPING * r[a] * w / out_weight[a];
        }
        r = next;
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::LinkOrigin;
    use crate::fan::expanded_entries;
    use katalogr_model::Provenance;

    fn entry(origin: u32, confidence: Confidence) -> LinkEntry {
        LinkEntry {
            origin: LinkOrigin::Ref(origin),
            confidence,
            span: None,
            provenance: Provenance {
                repo: "r".into(),
                path: katalogr_model::RelPath::new("a.tx").unwrap(),
                revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
            },
            fanout: None,
            produced_by: None,
        }
    }

    #[test]
    fn fan_weight_matches_expanded_row() {
        let rows = [
            LinkRow { entries: vec![] },
            LinkRow {
                entries: vec![entry(0, Confidence::Inferred)],
            },
            LinkRow {
                entries: vec![
                    entry(0, Confidence::Inferred),
                    entry(1, Confidence::Extracted),
                ],
            },
            LinkRow {
                entries: vec![
                    entry(0, Confidence::Inferred),
                    entry(1, Confidence::Inferred),
                ],
            },
        ];
        for row in &rows {
            for n in [0usize, 1, 2, 3, 7, 1000] {
                let expanded = LinkRow {
                    entries: expanded_entries(row, n),
                };
                assert_eq!(
                    fan_weight(row, n).to_bits(),
                    row_weight(&expanded).to_bits(),
                    "n = {n}, entries = {}",
                    row.entries.len()
                );
            }
        }
    }
}
