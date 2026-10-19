//! `rank`: PageRank over the reference graph (FEAT-025 section 6), used by `map`. The scores are
//! computed by [`katalogr_store::pagerank`]; when the cache holds a rank that reflects the
//! snapshot's watermark (FEAT-057) it is used instead, and the answer is identical.

use crate::resolve::hit;
use crate::result::SymbolHit;
use katalogr_store::{RANK_CONTEXT, Reader, StoreError, pagerank};

pub use katalogr_store::{link_weight, row_weight};

/// PageRank of every symbol, sorted by rank descending, then name. Same as
/// [`rank_cached_or_live`].
pub fn rank(tx: &impl Reader) -> Result<Vec<(SymbolHit, f64)>, StoreError> {
    rank_cached_or_live(tx)
}

/// The stored rank when it reflects the snapshot's watermark and covers every symbol, else the
/// rank computed now. Both give the same scores bit for bit.
pub fn rank_cached_or_live(tx: &impl Reader) -> Result<Vec<(SymbolHit, f64)>, StoreError> {
    let symbols = tx.symbols()?;
    if symbols.is_empty() {
        return Ok(Vec::new());
    }
    let cached = tx.cached_rank(RANK_CONTEXT)?.and_then(|m| {
        symbols
            .iter()
            .map(|(id, _)| m.get(id).copied())
            .collect::<Option<Vec<f64>>>()
    });
    let scores = match cached {
        Some(scores) => scores,
        None => pagerank(tx, &symbols)?,
    };
    Ok(sorted(symbols, scores))
}

/// The rank computed now, ignoring the stored one.
pub fn rank_live(tx: &impl Reader) -> Result<Vec<(SymbolHit, f64)>, StoreError> {
    let symbols = tx.symbols()?;
    let scores = pagerank(tx, &symbols)?;
    Ok(sorted(symbols, scores))
}

fn sorted(
    symbols: Vec<(katalogr_model::Id, katalogr_store::SymbolRow)>,
    scores: Vec<f64>,
) -> Vec<(SymbolHit, f64)> {
    let mut out: Vec<(SymbolHit, f64)> = symbols
        .iter()
        .zip(scores)
        .map(|((_, row), score)| (hit(row), score))
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.name.cmp(&b.0.name)));
    out
}
