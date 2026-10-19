//! `map`: an outline of the most important symbols.

use crate::rank::{rank, row_weight};
use crate::result::MapEntry;
use crate::search::round6;
use katalogr_model::Id;
use katalogr_store::{Reader, StoreError};
use std::collections::BTreeMap;

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Every symbol with its PageRank and the weighted number of links arriving at it (all kinds), by rank
/// descending, then name. In-degree is the sum of the weights of the link rows arriving at the
/// symbol (all kinds), rounded to 2 decimals.
pub fn map(tx: &impl Reader) -> Result<Vec<MapEntry>, StoreError> {
    let mut degree: BTreeMap<Id, f64> = BTreeMap::new();
    for (key, row) in tx.links_all()? {
        *degree.entry(key.to).or_default() += row_weight(&row);
    }
    Ok(rank(tx)?
        .into_iter()
        .map(|(symbol, score)| MapEntry {
            in_degree: round2(degree.get(&Id::of(&symbol.name)).copied().unwrap_or(0.0)),
            rank: round6(score),
            symbol,
        })
        .collect())
}
