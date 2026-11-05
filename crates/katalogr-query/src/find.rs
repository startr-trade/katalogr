//! `find`: symbols whose name matches text.

use crate::resolve::hits_of;
use crate::result::SymbolHit;
use katalogr_model::Id;
use katalogr_store::{Reader, StoreError};

/// Symbols whose canonical name equals `text` or whose plain (last-segment) name contains it,
/// ignoring case. Sorted by canonical name.
pub fn find(tx: &impl Reader, text: &str) -> Result<Vec<SymbolHit>, StoreError> {
    let lower = text.to_lowercase();
    let mut ids = tx.symbol_ids_plain_contains(&lower)?;
    let exact = Id::of(text);
    if !ids.contains(&exact) {
        ids.push(exact);
    }
    Ok(hits_of(tx, &ids)?
        .into_iter()
        .filter(|(h, n)| h.name == text || n.to_lowercase().contains(&lower))
        .map(|(h, _)| h)
        .collect())
}
