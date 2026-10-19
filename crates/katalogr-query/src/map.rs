//! `map`: an outline of the most referenced symbols.

use crate::resolve::hit;
use crate::result::MapEntry;
use katalogr_store::{Reader, StoreError};

/// Every symbol with the number of links arriving at it (all kinds), by that count descending,
/// then name. One scan of the incoming-link table.
pub fn map(tx: &impl Reader) -> Result<Vec<MapEntry>, StoreError> {
    let degree = tx.in_degrees()?;
    let mut out: Vec<MapEntry> = tx
        .symbols()?
        .into_iter()
        .map(|(id, row)| MapEntry {
            symbol: hit(&row),
            in_degree: degree.get(&id).copied().unwrap_or(0),
        })
        .collect();
    out.sort_by(|a, b| {
        (std::cmp::Reverse(a.in_degree), &a.symbol.name)
            .cmp(&(std::cmp::Reverse(b.in_degree), &b.symbol.name))
    });
    Ok(out)
}
