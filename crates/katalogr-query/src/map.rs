//! `map`: an outline of the most important symbols.

use crate::rank::rank;
use crate::result::MapEntry;
use crate::search::round6;
use katalogr_model::Id;
use katalogr_store::{Reader, StoreError};

/// Every symbol with its PageRank and the number of links arriving at it (all kinds), by rank
/// descending, then name. In-degree comes from one scan of the incoming-link table.
pub fn map(tx: &impl Reader) -> Result<Vec<MapEntry>, StoreError> {
    let degree = tx.in_degrees()?;
    Ok(rank(tx)?
        .into_iter()
        .map(|(symbol, score)| MapEntry {
            in_degree: degree.get(&Id::of(&symbol.name)).copied().unwrap_or(0),
            rank: round6(score),
            symbol,
        })
        .collect())
}
