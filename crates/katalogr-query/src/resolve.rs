//! Text to symbols.

use crate::result::{Resolution, SymbolHit};
use katalogr_model::{Id, LinkKind, Name};
use katalogr_store::{Reader, StoreError, SymbolRow};
use std::collections::BTreeSet;

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
        if !found.is_empty() {
            found = collapse_aliases(tx, found)?;
        }
        match found.len() {
            0 => continue,
            1 => return Ok(Resolution::One(found.remove(0))),
            _ => return Ok(Resolution::Ambiguous(found)),
        }
    }
    Ok(Resolution::None)
}

const ALIAS_KINDS: [&str; 2] = ["use", "reexport"];
const TYPE_KINDS: [&str; 4] = ["struct", "enum", "union", "trait"];

/// The symbol an alias finally re-exports, following single `reexports` links transitively.
/// `None` when the chain forks, dangles or loops, so the alias then stays as it is.
fn original_of(tx: &impl Reader, alias: &SymbolHit) -> Result<Option<SymbolHit>, StoreError> {
    let reexports = LinkKind("reexports".into());
    let mut seen = BTreeSet::new();
    let mut at = Id::of(&alias.name);
    seen.insert(at);
    loop {
        let out = tx.links_out(at, Some(&reexports))?;
        let [(key, _)] = out.as_slice() else {
            break;
        };
        if !seen.insert(key.to) {
            return Ok(None);
        }
        at = key.to;
    }
    if at == Id::of(&alias.name) {
        return Ok(None);
    }
    Ok(tx.symbol(at)?.map(|r| hit(&r)))
}

/// Merge candidates that are one thing to the user: a re-export alias goes to its original
/// (dropped if the original is a candidate, else replacing it), and an `impl` goes into the
/// type candidate in the same file that it `implements-for`. Order is kept, duplicates removed.
fn collapse_aliases(tx: &impl Reader, found: Vec<SymbolHit>) -> Result<Vec<SymbolHit>, StoreError> {
    let implements = LinkKind("implements-for".into());
    let mut out: Vec<SymbolHit> = Vec::new();
    for c in &found {
        let mut c = c.clone();
        if ALIAS_KINDS.contains(&c.kind.as_str()) {
            if let Some(orig) = original_of(tx, &c)? {
                c = orig;
            }
        } else if c.kind == "impl" {
            let mut into_type = false;
            for (key, _) in tx.links_out(Id::of(&c.name), Some(&implements))? {
                into_type |= found.iter().any(|t| {
                    TYPE_KINDS.contains(&t.kind.as_str())
                        && t.path == c.path
                        && Id::of(&t.name) == key.to
                });
            }
            if into_type {
                continue;
            }
        }
        if !out.iter().any(|o| o.name == c.name) {
            out.push(c);
        }
    }
    Ok(out)
}
