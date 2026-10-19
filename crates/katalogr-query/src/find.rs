//! `find`: symbols whose name matches text.

use crate::resolve::all_hits;
use crate::result::SymbolHit;
use katalogr_store::{Reader, StoreError};

/// Symbols whose canonical name equals `text` or whose plain (last-segment) name contains it,
/// ignoring case. Sorted by canonical name.
pub fn find(tx: &impl Reader, text: &str) -> Result<Vec<SymbolHit>, StoreError> {
    let lower = text.to_lowercase();
    Ok(all_hits(tx)?
        .into_iter()
        .filter(|(h, n)| h.name == text || n.to_lowercase().contains(&lower))
        .map(|(h, _)| h)
        .collect())
}
