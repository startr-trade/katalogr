//! `map`: an outline of the most important symbols.

use crate::diagnostic::{DIAG_TOP, DIAG_WEIGHTS, Diagnostic, RankSource, RoutingTrace};
use crate::rank::{rank_cached_or_live_with_source, row_weight};
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
    for (key, row) in tx.links_all()? {
        *degree.entry(key.to).or_default() += row_weight(&row);
    }
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
