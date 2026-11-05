//! `rank`: PageRank over the reference graph (FEAT-025 section 6), used by `map`. The scores are
//! computed by [`katalogr_store::pagerank`]; when the cache holds a rank that reflects the
//! snapshot's watermark (FEAT-057) it is used instead, and the answer is identical.

use crate::resolve::hit;
use crate::result::SymbolHit;
use katalogr_store::{RANK_CONTEXT, Reader, StoreError, SymbolCore, pagerank};

pub use katalogr_store::{link_weight, row_weight};

/// PageRank of every symbol, sorted by rank descending, then name. Same as
/// [`rank_cached_or_live`].
pub fn rank(tx: &impl Reader) -> Result<Vec<(SymbolHit, f64)>, StoreError> {
    rank_cached_or_live(tx)
}

/// The stored rank when it reflects the snapshot's watermark and covers every symbol, else the
/// rank computed now. Both give the same scores bit for bit.
pub fn rank_cached_or_live(tx: &impl Reader) -> Result<Vec<(SymbolHit, f64)>, StoreError> {
    Ok(rank_cached_or_live_with_source(tx)?.0)
}

/// [`rank_cached_or_live`], and whether the stored rank was the one served.
pub fn rank_cached_or_live_with_source(
    tx: &impl Reader,
) -> Result<(Vec<(SymbolHit, f64)>, bool), StoreError> {
    let cores = tx.symbol_cores()?;
    if cores.is_empty() {
        return Ok((Vec::new(), false));
    }
    let cached = tx.cached_rank(RANK_CONTEXT)?.and_then(|m| {
        cores
            .iter()
            .map(|(id, _)| m.get(id).copied())
            .collect::<Option<Vec<f64>>>()
    });
    let is_cached = cached.is_some();
    let scores = match cached {
        Some(scores) => scores,
        // Only a live rank needs the whole rows.
        None => pagerank(tx, &tx.symbols()?)?,
    };
    let hits = cores.into_iter().map(|(_, c)| core_hit(c)).collect();
    Ok((sorted(hits, scores), is_cached))
}

/// The hit of a symbol core.
pub(crate) fn core_hit(c: SymbolCore) -> SymbolHit {
    SymbolHit {
        name: c.name,
        kind: c.kind,
        path: c.path,
        span: c.span,
    }
}

/// The rank computed now, ignoring the stored one.
pub fn rank_live(tx: &impl Reader) -> Result<Vec<(SymbolHit, f64)>, StoreError> {
    let symbols = tx.symbols()?;
    let scores = pagerank(tx, &symbols)?;
    let hits = symbols.iter().map(|(_, row)| hit(row)).collect();
    Ok(sorted(hits, scores))
}

fn sorted(hits: Vec<SymbolHit>, scores: Vec<f64>) -> Vec<(SymbolHit, f64)> {
    let mut out: Vec<(SymbolHit, f64)> = hits.into_iter().zip(scores).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.name.cmp(&b.0.name)));
    out
}
