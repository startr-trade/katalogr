//! `refs`: everything that points at a symbol, including through re-exports.

use crate::diagnostic::{DIAG_REFS, Diagnostic, FoldTrace, WeightTrace};
use crate::fold::{Folded, fold};
use crate::resolve::hit;
use crate::result::{RefHit, SymbolHit};
use crate::search::round6;
use katalogr_model::{Confidence, ExportKey, Id};
use katalogr_store::{LinkKey, LinkRow, Reader, StoreError, link_weight, row_weight};
use std::collections::{BTreeSet, VecDeque};

const REEXPORT: &str = "reexports";

pub(crate) fn link_confidence(tx: &impl Reader, key: &LinkKey) -> Result<Confidence, StoreError> {
    tx.link(key)?
        .and_then(|r| r.entries.first().map(|e| e.confidence))
        .ok_or_else(|| StoreError::Row("link without entries".into()))
}

/// The rule that produced a link (`profile:<name>#<id>`), when one did.
pub(crate) fn link_origin(tx: &impl Reader, key: &LinkKey) -> Result<Option<String>, StoreError> {
    Ok(tx
        .link(key)?
        .and_then(|r| r.entries.into_iter().find_map(|e| e.produced_by)))
}

/// Links arriving at `target` (any kind), plus those arriving at anything that re-exports it,
/// transitively. Each hit names the re-export chain it came through. Cycle-safe. Sorted by
/// chain length, then referrer name, then link kind.
pub fn refs(tx: &impl Reader, target: &SymbolHit) -> Result<Vec<RefHit>, StoreError> {
    refs_traced(tx, target, &mut Diagnostic::off())
}

/// The weight line of the link `key`: the entry that gives its row weight.
pub(crate) fn weight_trace(
    to: &str,
    from: &str,
    key: &LinkKey,
    row: &LinkRow,
) -> Option<WeightTrace> {
    let mut best: Option<(&katalogr_store::LinkEntry, f64)> = None;
    for e in &row.entries {
        let w = link_weight(e);
        if best.is_none_or(|(_, b)| w > b) {
            best = Some((e, w));
        }
    }
    let (entry, _) = best?;
    Some(WeightTrace {
        to: to.to_string(),
        from: from.to_string(),
        kind: key.kind.0.clone(),
        confidence: format!("{:?}", entry.confidence),
        fanout: entry.fanout,
        weight: round6(row_weight(row)),
    })
}

/// What [`refs`] does, recording the link weights of the leading hits into `diag`. The answer
/// does not depend on `diag`.
pub fn refs_traced(
    tx: &impl Reader,
    target: &SymbolHit,
    diag: &mut Diagnostic,
) -> Result<Vec<RefHit>, StoreError> {
    Ok(refs_keyed(tx, target, diag)?
        .into_iter()
        .map(|(h, _)| h)
        .collect())
}

/// [`refs_traced`], each hit with the link it arrived by (from the referrer to the node the
/// hit arrived at: the target or a re-exporter).
pub(crate) fn refs_keyed(
    tx: &impl Reader,
    target: &SymbolHit,
    diag: &mut Diagnostic,
) -> Result<Vec<(RefHit, LinkKey)>, StoreError> {
    let root = Id::of(&target.name);
    let mut seen = BTreeSet::from([root]);
    let mut queue = VecDeque::from([(root, Vec::<String>::new())]);
    let mut out: Vec<(RefHit, LinkKey)> = Vec::new();
    while let Some((id, via)) = queue.pop_front() {
        for key in tx.links_in(id)? {
            let Some(row) = tx.symbol(key.from)? else {
                continue;
            };
            let from = hit(&row);
            if key.kind.0 == REEXPORT && seen.insert(key.from) {
                let mut next = via.clone();
                next.push(from.name.clone());
                queue.push_back((key.from, next));
            }
            out.push((
                RefHit {
                    confidence: link_confidence(tx, &key)?,
                    kind: key.kind.0.clone(),
                    from,
                    via: via.clone(),
                },
                key,
            ));
        }
    }
    out.sort_by(|a, b| {
        (a.0.via.len(), &a.0.from.name, &a.0.kind).cmp(&(b.0.via.len(), &b.0.from.name, &b.0.kind))
    });
    if let Some(d) = diag.on() {
        for (h, key) in out.iter().take(DIAG_REFS) {
            let to_name = match tx.symbol(key.to)? {
                Some(row) => hit(&row).name,
                None => target.name.clone(),
            };
            if let Some(row) = tx.link(key)?
                && let Some(w) = weight_trace(&to_name, &h.from.name, key, &row)
            {
                d.weights.push(w);
            }
        }
        d.add_omitted("weights", out.len().saturating_sub(DIAG_REFS) as u64);
    }
    Ok(out)
}

/// [`refs_traced`], then folded with `fold_from` (None: no folding, `hits` is the whole
/// answer). The trace's weights come from the unfolded answer; each fold adds a `FoldTrace`.
pub fn refs_folded_traced(
    tx: &impl Reader,
    target: &SymbolHit,
    fold_from: Option<u32>,
    diag: &mut Diagnostic,
) -> Result<Folded<RefHit>, StoreError> {
    let keyed = refs_keyed(tx, target, diag)?;
    let keyed: Vec<(RefHit, Option<LinkKey>)> =
        keyed.into_iter().map(|(h, k)| (h, Some(k))).collect();
    let Some(from) = fold_from else {
        return Ok(Folded {
            hits: keyed.into_iter().map(|(h, _)| h).collect(),
            folds: Vec::new(),
        });
    };
    let folded = fold(
        tx,
        keyed,
        from,
        |h: &RefHit, k: &ExportKey| (h.via.clone(), h.kind.clone(), k.clone()),
        |h| h.from.clone(),
        |g, f| {
            f.via = g.0.clone();
            f.kind = g.1.clone();
        },
    )?;
    if let Some(d) = diag.on() {
        for f in folded.folds.iter().take(DIAG_REFS) {
            d.folds.push(FoldTrace {
                key: f.key.clone(),
                kind: f.kind.clone(),
                targets: f.targets,
                hits: f.hits,
                at: f.at,
            });
        }
        d.add_omitted("folds", folded.folds.len().saturating_sub(DIAG_REFS) as u64);
    }
    Ok(folded)
}
