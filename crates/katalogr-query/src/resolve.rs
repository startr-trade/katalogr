//! Text to symbols.

use crate::result::{Resolution, SymbolHit};
use katalogr_model::Name;
use katalogr_store::{Reader, StoreError, SymbolRow};

pub(crate) fn hit(row: &SymbolRow) -> SymbolHit {
    SymbolHit {
        name: row.name.canonical(),
        kind: row.kind.clone(),
        path: row.name.artifact.path.as_str().to_string(),
        span: row.span,
    }
}

fn plain_name(row: &SymbolRow) -> &str {
    row.name.segments.last().map_or("", |s| s.name.as_str())
}

/// Every symbol, sorted by canonical name.
pub(crate) fn all_hits(tx: &impl Reader) -> Result<Vec<(SymbolHit, String)>, StoreError> {
    let mut out: Vec<(SymbolHit, String)> = tx
        .symbols()?
        .into_iter()
        .map(|(_, r)| (hit(&r), plain_name(&r).to_string()))
        .collect();
    out.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    Ok(out)
}

/// The symbols `text` names, by the first tier that matches: exact canonical name, exact plain
/// name, then plain name ignoring case.
pub fn resolve(tx: &impl Reader, text: &str) -> Result<Resolution<SymbolHit>, StoreError> {
    let all = all_hits(tx)?;
    let lower = text.to_lowercase();
    for tier in 0..3 {
        let mut found: Vec<SymbolHit> = all
            .iter()
            .filter(|(h, n)| match tier {
                0 => h.name == text,
                1 => n == text,
                _ => n.to_lowercase() == lower,
            })
            .map(|(h, _)| h.clone())
            .collect();
        match found.len() {
            0 => continue,
            1 => return Ok(Resolution::One(found.remove(0))),
            _ => return Ok(Resolution::Ambiguous(found)),
        }
    }
    Ok(Resolution::None)
}
