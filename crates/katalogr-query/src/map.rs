//! `map`: an outline of the most important symbols.

use crate::diagnostic::{DIAG_TOP, DIAG_WEIGHTS, Diagnostic, RankSource, RoutingTrace};
use crate::rank::rank_cached_or_live_with_source;
use crate::refs::weight_trace;
use crate::result::MapEntry;
use crate::search::round6;
use katalogr_model::Id;
use katalogr_store::{PAGERANK_DAMPING, PAGERANK_ITERATIONS, RANK_CONTEXT, Reader, StoreError};
use std::collections::BTreeMap;

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Every symbol with its PageRank and the weighted number of links arriving at it (all kinds), by rank
/// descending, then name. In-degree is the sum of the weights of the link rows arriving at the
/// symbol (all kinds), rounded to 2 decimals.
pub fn map(tx: &impl Reader) -> Result<Vec<MapEntry>, StoreError> {
    map_traced(tx, &mut Diagnostic::off())
}

/// What [`map`] does, recording the rank source and the heaviest incoming links of the top
/// symbols into `diag`. The answer does not depend on `diag`.
pub fn map_traced(tx: &impl Reader, diag: &mut Diagnostic) -> Result<Vec<MapEntry>, StoreError> {
    let mut degree: BTreeMap<Id, f64> = BTreeMap::new();
    tx.for_each_link(&mut |_, _, to, weight, _| {
        *degree.entry(to).or_default() += weight;
    })?;
    let (ranked, cached) = rank_cached_or_live_with_source(tx)?;
    if let Some(d) = diag.on() {
        d.routing = Some(RoutingTrace {
            command: "map".into(),
            input: String::new(),
            kind: "none".into(),
            tokens: Vec::new(),
            dropped: Vec::new(),
            tier: None,
            candidates: 0,
            after_collapse: 0,
        });
        d.rank_source = Some(RankSource {
            source: if cached { "cached" } else { "live" }.into(),
            watermark: tx.watermark(RANK_CONTEXT)?,
            rank_watermark: tx.rank_watermark()?,
            damping: PAGERANK_DAMPING,
            iterations: PAGERANK_ITERATIONS,
        });
        for (symbol, _) in ranked.iter().take(DIAG_TOP) {
            let to = Id::of(&symbol.name);
            let mut links = Vec::new();
            for key in tx.links_in(to)? {
                let Some(row) = tx.link(&key)? else { continue };
                let from = match tx.symbol(key.from)? {
                    Some(r) => crate::resolve::hit(&r).name,
                    None => key.from.to_hex(),
                };
                if let Some(w) = weight_trace(&symbol.name, &from, &key, &row) {
                    links.push(w);
                }
            }
            links.sort_by(|a, b| {
                b.weight
                    .total_cmp(&a.weight)
                    .then_with(|| a.from.cmp(&b.from))
            });
            d.cap(&mut links, DIAG_WEIGHTS, "weights");
            d.weights.extend(links);
        }
    }
    Ok(ranked
        .into_iter()
        .map(|(symbol, score)| MapEntry {
            in_degree: round2(degree.get(&Id::of(&symbol.name)).copied().unwrap_or(0.0)),
            rank: round6(score),
            symbol,
        })
        .collect())
}

/// The start of [`map`], from the stored top of the rank, with `in_degree` 0.0: only for callers
/// that do not read `in_degree`. Returns the number of symbols and the entries; `None` when the
/// stored rank is not current or a row is missing, and the caller then uses [`map`].
pub fn map_top(tx: &impl Reader) -> Result<Option<(u64, Vec<MapEntry>)>, StoreError> {
    if !tx.rank_is_current(RANK_CONTEXT)? {
        return Ok(None);
    }
    let Some(top) = tx.rank_top()? else {
        return Ok(None);
    };
    let ids: Vec<Id> = top.entries.iter().map(|(id, _)| *id).collect();
    let rows = tx.symbols_many(&ids)?;
    let mut out = Vec::with_capacity(rows.len());
    for (row, (_, score)) in rows.iter().zip(&top.entries) {
        let Some(row) = row else { return Ok(None) };
        out.push(MapEntry {
            symbol: crate::resolve::hit(row),
            rank: round6(*score),
            in_degree: 0.0,
        });
    }
    Ok(Some((top.total, out)))
}

/// As [`map_top`], for an answer already marked stale (FEAT-080 R-7): the stored head of the
/// rank is used whether or not it is current, and symbols no longer present are skipped. `None`
/// when no head is stored. Only for `session-start` during a drain; [`map`] stays exact.
pub fn map_top_behind(tx: &impl Reader) -> Result<Option<(u64, Vec<MapEntry>)>, StoreError> {
    let Some(top) = tx.rank_top()? else {
        return Ok(None);
    };
    let ids: Vec<Id> = top.entries.iter().map(|(id, _)| *id).collect();
    let rows = tx.symbols_many(&ids)?;
    let mut out = Vec::with_capacity(rows.len());
    for (row, (_, score)) in rows.iter().zip(&top.entries) {
        let Some(row) = row else { continue };
        out.push(MapEntry {
            symbol: crate::resolve::hit(row),
            rank: round6(*score),
            in_degree: 0.0,
        });
    }
    Ok(Some((top.total, out)))
}
