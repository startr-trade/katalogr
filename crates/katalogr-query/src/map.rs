//! `map`: an outline of the most referenced symbols.

use crate::resolve::hit;
use crate::result::MapEntry;
use katalogr_model::Id;
use katalogr_store::cache::TableId;
use katalogr_store::{Reader, StoreError};
use std::collections::BTreeMap;

/// Every symbol with the number of links arriving at it (all kinds), by that count descending,
/// then name. One scan of the incoming-link table.
pub fn map(tx: &impl Reader) -> Result<Vec<MapEntry>, StoreError> {
    let mut degree: BTreeMap<[u8; 16], u64> = BTreeMap::new();
    for (key, _) in tx.raw_scan(TableId::LinksIn, &[])? {
        let target: [u8; 16] = key
            .get(..16)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| StoreError::Row("malformed key".into()))?;
        *degree.entry(target).or_default() += 1;
    }
    let mut out: Vec<MapEntry> = tx
        .symbols()?
        .into_iter()
        .map(|(Id(id), row)| MapEntry {
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
