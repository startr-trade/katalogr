//! Input to seeds for `impact` (FEAT-028): a symbol, a file, a set of changed lines or a board
//! item becomes the nodes the walk starts from. Pure reads of one snapshot.

use crate::doc_impact::{ChangedFile, names_path};
use crate::impact::{SEED_BROKEN_REF, SEED_CONTRACT, SEED_FILE, SEED_ITEM, SEED_SYMBOL, Seed};
use crate::resolve::CONTRACT_KIND;
use crate::result::SymbolHit;
use katalogr_model::annotate::ITEM_NS;
use katalogr_model::{ArtifactName, ExportKey, Id, LinkKind, Name, RelPath, Span};
use katalogr_store::{Reader, StoreError};
use std::collections::BTreeSet;

const ANNOTATED_BY: &str = "annotated-by";
const REFS_ITEM: &str = "refs-item";
const TOUCHES: &str = "touches";
const CONTAINS: &str = "contains";

fn seed(id: Id, precision: &'static str) -> Seed {
    Seed {
        id,
        precision,
        wanted: None,
    }
}

/// The symbol, or the contract when `target` is one.
pub fn seeds_for_symbol(target: &SymbolHit) -> Vec<Seed> {
    let precision = if target.kind == CONTRACT_KIND {
        SEED_CONTRACT
    } else {
        SEED_SYMBOL
    };
    vec![seed(Id::of(&target.name), precision)]
}

/// The artifact and its symbols, all at `file`; `None` when the file is not indexed.
pub fn seeds_for_file(
    tx: &impl Reader,
    repo: &str,
    path: &RelPath,
) -> Result<Option<Vec<Seed>>, StoreError> {
    let id = ArtifactName {
        repo: repo.to_string(),
        path: path.clone(),
    }
    .id();
    let Some(row) = tx.artifact(id)? else {
        return Ok(None);
    };
    let mut out = vec![seed(id, SEED_FILE)];
    out.extend(row.symbols.iter().map(|&s| seed(s, SEED_FILE)));
    Ok(Some(out))
}

/// The symbols a changed range lands on, without their containers: of the symbols overlapping
/// `range` (1-based lines, inclusive), a symbol S is dropped when another overlapping symbol T
/// lies inside S and `range` lies wholly inside T. Symbols with equal spans do not nest.
pub fn innermost(symbols: &[(Id, Span)], range: (u32, u32)) -> Vec<Id> {
    let (a, b) = range;
    let overlapping: Vec<&(Id, Span)> = symbols
        .iter()
        .filter(|(_, s)| s.start_line <= b && a <= s.end_line)
        .collect();
    let start = |s: &Span| (s.start_line, s.start_col);
    let end = |s: &Span| (s.end_line, s.end_col);
    overlapping
        .iter()
        .filter(|(id, s)| {
            !overlapping.iter().any(|(tid, t)| {
                tid != id
                    && t != s
                    && start(t) >= start(s)
                    && end(t) <= end(s)
                    && t.start_line <= a
                    && b <= t.end_line
            })
        })
        .map(|(id, _)| *id)
        .collect()
}

/// Innermost symbols of each changed line, else the file; then broken references of deleted
/// files. An added file seeds its artifact and symbols at `file`. The result is sorted and has
/// no duplicates.
pub fn seeds_for_changes(
    tx: &impl Reader,
    repo: &str,
    changes: &[ChangedFile],
) -> Result<Vec<Seed>, StoreError> {
    let mut out: BTreeSet<Seed> = BTreeSet::new();
    let mut deleted: Vec<&ChangedFile> = Vec::new();
    for c in changes {
        if c.kind == "deleted" {
            deleted.push(c);
            continue;
        }
        let artifact = ArtifactName {
            repo: repo.to_string(),
            path: c.path.clone(),
        }
        .id();
        let Some(row) = tx.artifact(artifact)? else {
            continue;
        };
        if c.kind != "modified" {
            out.insert(seed(artifact, SEED_FILE));
            out.extend(row.symbols.iter().map(|&s| seed(s, SEED_FILE)));
            continue;
        }
        let mut symbols: Vec<(Id, Span)> = Vec::new();
        for (id, sym) in row.symbols.iter().zip(tx.symbols_many(&row.symbols)?) {
            if let Some(sym) = sym {
                symbols.push((*id, sym.span));
            }
        }
        let mut found = false;
        for &range in &c.ranges {
            for id in innermost(&symbols, range) {
                found = true;
                out.insert(seed(id, SEED_SYMBOL));
            }
        }
        if !found {
            out.insert(seed(artifact, SEED_FILE));
        }
    }
    if !deleted.is_empty() {
        for (_, r) in tx.unresolved_refs(None)? {
            for c in &deleted {
                for cand in &r.candidates {
                    if names_path(&cand.key, c.path.as_str()) {
                        out.insert(Seed {
                            id: r.from,
                            precision: SEED_BROKEN_REF,
                            wanted: Some(format!("{}:{}", cand.namespace, cand.key)),
                        });
                    }
                }
            }
        }
    }
    Ok(out.into_iter().collect())
}

/// The sources of the `annotated-by` links into `id`.
fn annotators(tx: &impl Reader, id: Id) -> Result<Vec<Id>, StoreError> {
    Ok(tx
        .links_in(id)?
        .into_iter()
        .filter(|k| k.kind.0 == ANNOTATED_BY)
        .map(|k| k.from)
        .collect())
}

/// What annotates the item, and the files touched by what refers to it, at `item`; the same
/// for each requirement directly under it. `None` when no node exports the code in the item
/// namespace; an item with no code linked gives `Some` of an empty list.
pub fn seeds_for_item(tx: &impl Reader, code: &str) -> Result<Option<Vec<Seed>>, StoreError> {
    let items = tx.exports(&ExportKey {
        namespace: ITEM_NS.to_string(),
        key: code.to_string(),
    })?;
    if items.is_empty() {
        return Ok(None);
    }
    let mut ends: Vec<Id> = items.clone();
    let contains = LinkKind(CONTAINS.to_string());
    for &item in &items {
        ends.extend(tx.link_keys_out(item, &contains)?.into_iter().map(|k| k.to));
    }
    let touches = LinkKind(TOUCHES.to_string());
    let mut out: BTreeSet<Seed> = BTreeSet::new();
    for &end in &ends {
        for from in annotators(tx, end)? {
            out.insert(seed(from, SEED_ITEM));
        }
    }
    for &item in &items {
        for key in tx.links_in(item)? {
            if key.kind.0 != REFS_ITEM || tx.artifact(key.from)?.is_none() {
                continue;
            }
            for t in tx.link_keys_out(key.from, &touches)? {
                let Some(file) = tx.artifact(t.to)? else {
                    continue;
                };
                if let Some(seeds) = seeds_for_file(tx, &file.name.repo, &file.name.path)? {
                    out.extend(seeds.into_iter().map(|s| seed(s.id, SEED_ITEM)));
                }
            }
        }
    }
    Ok(Some(out.into_iter().collect()))
}
