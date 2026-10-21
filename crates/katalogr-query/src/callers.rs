//! `callers`: who calls a symbol, transitively.

use crate::fold::{Folded, fold};
use crate::refs::link_confidence;
use crate::resolve::hit;
use crate::result::{CallerHit, SymbolHit};
use crate::step::incoming;
use katalogr_model::{ExportKey, Id};
use katalogr_store::{LinkKey, Reader, StoreError};
use std::collections::BTreeSet;

/// Breadth-first over incoming `calls` links, up to `depth` (at least 1). A provider of a
/// contract is also called by the contract's consumers (`via` names the contract), and so is
/// the contract itself when it is the target. Each caller appears once, at its smallest depth.
/// Sorted by depth, then name.
pub fn callers(
    tx: &impl Reader,
    target: &SymbolHit,
    depth: u32,
) -> Result<Vec<CallerHit>, StoreError> {
    Ok(callers_keyed(tx, target, depth)?
        .into_iter()
        .map(|(h, _)| h)
        .collect())
}

/// [`callers`], each hit with the `calls` link of its step (None for a step through a contract).
pub(crate) fn callers_keyed(
    tx: &impl Reader,
    target: &SymbolHit,
    depth: u32,
) -> Result<Vec<(CallerHit, Option<LinkKey>)>, StoreError> {
    let root = Id::of(&target.name);
    let mut seen = BTreeSet::from([root]);
    let mut frontier = vec![root];
    let mut out = Vec::new();
    for d in 1..=depth.max(1) {
        let mut next = Vec::new();
        for id in frontier {
            for step in incoming(tx, id)? {
                if !seen.insert(step.from) {
                    continue;
                }
                let Some(row) = tx.symbol(step.from)? else {
                    continue;
                };
                let link = step.via.is_none().then(|| step.key.clone());
                out.push((
                    CallerHit {
                        confidence: link_confidence(tx, &step.key)?,
                        caller: hit(&row),
                        depth: d,
                        via: step.via.map(|(name, _)| name),
                    },
                    link,
                ));
                next.push(step.from);
            }
        }
        frontier = next;
    }
    out.sort_by(|a, b| (a.0.depth, &a.0.caller.name).cmp(&(b.0.depth, &b.0.caller.name)));
    Ok(out)
}

/// A `callers` answer for the facade: the hits within `depth`, folded, and whether a caller
/// exists one level deeper.
#[derive(Debug, Clone, PartialEq)]
pub struct CallersAnswer {
    pub folded: Folded<CallerHit>,
    /// A caller exists beyond `depth` (the depth probe).
    pub deeper: bool,
    /// Hits within `depth` before folding (the `shown` of a depth `partial`).
    pub unfolded_len: usize,
}

/// Traverses `depth + 1` levels as [`callers`] does (folded callers are still followed), sets
/// `deeper`, keeps the hits within `depth`, then folds them with `fold_from` (None: no folding).
pub fn callers_folded(
    tx: &impl Reader,
    target: &SymbolHit,
    depth: u32,
    fold_from: Option<u32>,
) -> Result<CallersAnswer, StoreError> {
    let depth = depth.max(1);
    let mut keyed = callers_keyed(tx, target, depth.saturating_add(1))?;
    let deeper = keyed.iter().any(|(h, _)| h.depth > depth);
    keyed.retain(|(h, _)| h.depth <= depth);
    let unfolded_len = keyed.len();
    let folded = match fold_from {
        None => Folded {
            hits: keyed.into_iter().map(|(h, _)| h).collect(),
            folds: Vec::new(),
        },
        Some(from) => fold(
            tx,
            keyed,
            from,
            |h: &CallerHit, k: &ExportKey| (h.depth, k.clone()),
            |h| h.caller.clone(),
            |g, f| {
                f.depth = Some(g.0);
                f.kind = "calls".to_string();
            },
        )?,
    };
    Ok(CallersAnswer {
        folded,
        deeper,
        unfolded_len,
    })
}
