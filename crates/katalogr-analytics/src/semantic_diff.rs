//! Semantic diff between two graph snapshots (FEAT-095 R-3, R-4, R-5, R-7): the symbols that
//! came, went, moved or changed, the link and contract differences, and every cap that cut a
//! list.
//!
//! Pure over two readers. Names, never ids, appear in the output, and every list is sorted.

use crate::context_diff::{ContextDiff, context_diff, contract_shapes, name_of};
use katalogr_model::{Fact, Id, Name};
use katalogr_query::result::{Partial, cap};
use katalogr_store::{Reader, StoreError, SymbolRow};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// What to compare and how much to list.
#[derive(Clone, Debug)]
pub struct DiffOptions {
    /// Entries per list.
    pub limit: usize,
    /// Artifacts whose path starts with one of these are left out. The wrapper passes its history prefix.
    pub exclude_path_prefixes: Vec<String>,
}

/// A symbol by its canonical name, kind, repository-relative path and first line.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct SymbolRef {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line: u32,
}

/// One symbol that is in another file on side B.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct SymbolMove {
    pub from: SymbolRef,
    pub to: SymbolRef,
}

/// The same symbol on both sides, differing in its `signature` fact or its kind.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct SymbolChange {
    pub symbol: SymbolRef,
    pub signature_a: Option<String>,
    pub signature_b: Option<String>,
    pub kind_a: String,
}

/// Symbol differences, each list capped and its total kept.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SymbolChanges {
    pub added: Vec<SymbolRef>,
    pub added_total: u64,
    pub removed: Vec<SymbolRef>,
    pub removed_total: u64,
    pub moved: Vec<SymbolMove>,
    pub moved_total: u64,
    pub changed: Vec<SymbolChange>,
    pub changed_total: u64,
    /// Files compared because their blob differs or they exist on one side.
    pub files_changed: u64,
}

/// A contract's providers, consumers and declarers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractCounts {
    pub providers: u32,
    pub consumers: u32,
    pub declarers: u32,
}

/// One contract whose shape differs between the sides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractChange {
    pub contract: String,
    pub a: ContractCounts,
    pub b: ContractCounts,
    pub consumers_added: Vec<String>,
    pub consumers_removed: Vec<String>,
    pub providers_added: Vec<String>,
    pub providers_removed: Vec<String>,
    pub declarers_added: Vec<String>,
    pub declarers_removed: Vec<String>,
    pub newly_dangling: bool,
    pub no_longer_dangling: bool,
}

/// Everything that differs between two snapshots, outside the insight shifts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SemanticDiff {
    pub symbols: SymbolChanges,
    /// Exactly `context_diff`'s answer.
    pub links: ContextDiff,
    pub contracts: Vec<ContractChange>,
    pub contracts_total: u64,
    /// Every cap that cut a list, in field order. Stored, because a report's limit is in its ID.
    pub partial: Vec<Partial>,
}

/// The blob of a file revision: the text up to the first `+`.
fn blob(revision: &str) -> &str {
    revision.split('+').next().unwrap_or(revision)
}

fn excluded(path: &str, o: &DiffOptions) -> bool {
    o.exclude_path_prefixes
        .iter()
        .any(|p| path.starts_with(p.as_str()))
}

/// Cut `v` to `limit`, recording the cap when it cuts. Returns the list and its full length.
fn cut<T>(mut v: Vec<T>, limit: usize, partial: &mut Vec<Partial>) -> (Vec<T>, u64) {
    let total = v.len() as u64;
    if v.len() > limit {
        partial.push(Partial::new(
            cap::LIMIT,
            limit as u64,
            limit as u64,
            Some(total),
        ));
        v.truncate(limit);
    }
    (v, total)
}

fn sym_ref(row: &SymbolRow) -> SymbolRef {
    SymbolRef {
        name: row.name.canonical(),
        kind: row.kind.clone(),
        path: row.name.artifact.path.as_str().to_string(),
        line: row.span.start_line,
    }
}

fn signature(row: &SymbolRow) -> Option<&Fact> {
    row.facts.get("signature")
}

fn fact_text(f: &Fact) -> String {
    match f {
        Fact::Str(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// What identifies a symbol apart from the file it is in.
type MoveKey = (String, Vec<(String, String)>, u32);

fn move_key(row: &SymbolRow) -> MoveKey {
    (
        row.kind.clone(),
        row.name
            .segments
            .iter()
            .map(|s| (s.kind.clone(), s.name.clone()))
            .collect(),
        row.name.ordinal,
    )
}

/// Artifacts by id: their path and blob, leaving out excluded paths.
type Files = BTreeMap<Id, (String, String, Vec<Id>)>;

fn files(r: &impl Reader, o: &DiffOptions) -> Result<Files, StoreError> {
    Ok(r.artifacts()?
        .into_iter()
        .filter(|(_, a)| !excluded(a.name.path.as_str(), o))
        .map(|(id, a)| {
            (
                id,
                (
                    a.name.path.as_str().to_string(),
                    blob(&a.provenance.revision).to_string(),
                    a.symbols,
                ),
            )
        })
        .collect())
}

fn read_symbols(r: &impl Reader, ids: &[Id]) -> Result<BTreeMap<Id, SymbolRow>, StoreError> {
    let rows = r.symbols_many(ids)?;
    Ok(ids
        .iter()
        .zip(rows)
        .filter_map(|(id, row)| row.map(|r| (*id, r)))
        .collect())
}

fn symbol_changes(
    a: &impl Reader,
    b: &impl Reader,
    o: &DiffOptions,
    partial: &mut Vec<Partial>,
) -> Result<SymbolChanges, StoreError> {
    let (fa, fb) = (files(a, o)?, files(b, o)?);
    let mut ids_a: BTreeSet<Id> = BTreeSet::new();
    let mut ids_b: BTreeSet<Id> = BTreeSet::new();
    let mut files_changed = 0u64;
    for (id, (_, blob_a, syms_a)) in &fa {
        match fb.get(id) {
            Some((_, blob_b, _)) if blob_a == blob_b => {}
            other => {
                files_changed += 1;
                ids_a.extend(syms_a.iter().copied());
                if let Some((_, _, syms_b)) = other {
                    ids_b.extend(syms_b.iter().copied());
                }
            }
        }
    }
    for (id, (_, _, syms_b)) in &fb {
        if !fa.contains_key(id) {
            files_changed += 1;
            ids_b.extend(syms_b.iter().copied());
        }
    }
    let ids_a: Vec<Id> = ids_a.into_iter().collect();
    let ids_b: Vec<Id> = ids_b.into_iter().collect();
    let (ra, rb) = (read_symbols(a, &ids_a)?, read_symbols(b, &ids_b)?);

    let mut added: Vec<&SymbolRow> = Vec::new();
    let mut removed: Vec<&SymbolRow> = Vec::new();
    let mut changed: Vec<SymbolChange> = Vec::new();
    for (id, row) in &rb {
        match ra.get(id) {
            None => added.push(row),
            Some(old) => {
                if old.kind != row.kind || signature(old) != signature(row) {
                    changed.push(SymbolChange {
                        symbol: sym_ref(row),
                        signature_a: signature(old).map(fact_text),
                        signature_b: signature(row).map(fact_text),
                        kind_a: old.kind.clone(),
                    });
                }
            }
        }
    }
    for (id, row) in &ra {
        if !rb.contains_key(id) {
            removed.push(row);
        }
    }

    // A move is a key that occurs exactly once among the removed and once among the added.
    let mut by_key_removed: BTreeMap<MoveKey, Vec<usize>> = BTreeMap::new();
    let mut by_key_added: BTreeMap<MoveKey, Vec<usize>> = BTreeMap::new();
    for (i, r) in removed.iter().enumerate() {
        by_key_removed.entry(move_key(r)).or_default().push(i);
    }
    for (i, r) in added.iter().enumerate() {
        by_key_added.entry(move_key(r)).or_default().push(i);
    }
    let mut moved: Vec<SymbolMove> = Vec::new();
    let mut gone_removed: BTreeSet<usize> = BTreeSet::new();
    let mut gone_added: BTreeSet<usize> = BTreeSet::new();
    for (key, ri) in &by_key_removed {
        let (Some(ai), [r]) = (by_key_added.get(key), ri.as_slice()) else {
            continue;
        };
        let [ad] = ai.as_slice() else { continue };
        let (from, to) = (sym_ref(removed[*r]), sym_ref(added[*ad]));
        if from.path == to.path && removed[*r].name.artifact == added[*ad].name.artifact {
            continue;
        }
        moved.push(SymbolMove { from, to });
        gone_removed.insert(*r);
        gone_added.insert(*ad);
    }

    let mut added: Vec<SymbolRef> = added
        .iter()
        .enumerate()
        .filter(|(i, _)| !gone_added.contains(i))
        .map(|(_, r)| sym_ref(r))
        .collect();
    let mut removed: Vec<SymbolRef> = removed
        .iter()
        .enumerate()
        .filter(|(i, _)| !gone_removed.contains(i))
        .map(|(_, r)| sym_ref(r))
        .collect();
    added.sort();
    removed.sort();
    moved.sort();
    changed.sort();

    let limit = o.limit;
    let (added, added_total) = cut(added, limit, partial);
    let (removed, removed_total) = cut(removed, limit, partial);
    let (moved, moved_total) = cut(moved, limit, partial);
    let (changed, changed_total) = cut(changed, limit, partial);
    Ok(SymbolChanges {
        added,
        added_total,
        removed,
        removed_total,
        moved,
        moved_total,
        changed,
        changed_total,
        files_changed,
    })
}

fn counts(r: &impl Reader, id: Id) -> Result<ContractCounts, StoreError> {
    Ok(r.contract(id)?
        .map_or_else(ContractCounts::default, |c| ContractCounts {
            providers: c.providers,
            consumers: c.consumers,
            declarers: c.declarers,
        }))
}

fn dangling(c: &ContractCounts) -> bool {
    (c.providers > 0) != (c.consumers > 0)
}

/// Names bound in `x` with `role` and not in `y`, sorted.
fn bound_not_in(
    x: &BTreeSet<(Id, String)>,
    y: &BTreeSet<(Id, String)>,
    role: &str,
    names: &dyn Fn(Id) -> String,
) -> Vec<String> {
    let mut v: Vec<String> = x
        .iter()
        .filter(|(_, r)| r == role)
        .filter(|b| !y.contains(*b))
        .map(|(s, _)| names(*s))
        .collect();
    v.sort();
    v.dedup();
    v
}

fn contract_changes(
    a: &impl Reader,
    b: &impl Reader,
    limit: usize,
    partial: &mut Vec<Partial>,
) -> Result<(Vec<ContractChange>, u64), StoreError> {
    let (sa, sb) = (contract_shapes(a)?, contract_shapes(b)?);
    let empty = BTreeSet::new();
    let names = |id: Id| name_of(id, &[a, b]);
    let mut out: Vec<ContractChange> = Vec::new();
    let mut seen: BTreeSet<Id> = BTreeSet::new();
    for id in sa.keys().chain(sb.keys()) {
        if !seen.insert(*id) || sa.get(id) == sb.get(id) {
            continue;
        }
        let (ca, cb) = (counts(a, *id)?, counts(b, *id)?);
        let (ba, bb) = (
            sa.get(id).map_or(&empty, |e| &e.1),
            sb.get(id).map_or(&empty, |e| &e.1),
        );
        let mut cap_list = |v: Vec<String>| cut(v, limit, partial).0;
        let consumers_added = cap_list(bound_not_in(bb, ba, "Consumes", &names));
        let consumers_removed = cap_list(bound_not_in(ba, bb, "Consumes", &names));
        let providers_added = cap_list(bound_not_in(bb, ba, "Provides", &names));
        let providers_removed = cap_list(bound_not_in(ba, bb, "Provides", &names));
        let declarers_added = cap_list(bound_not_in(bb, ba, "Declares", &names));
        let declarers_removed = cap_list(bound_not_in(ba, bb, "Declares", &names));
        let (in_a, in_b) = (sa.contains_key(id), sb.contains_key(id));
        out.push(ContractChange {
            contract: names(*id),
            a: ca,
            b: cb,
            consumers_added,
            consumers_removed,
            providers_added,
            providers_removed,
            declarers_added,
            declarers_removed,
            newly_dangling: in_b && dangling(&cb) && !(in_a && dangling(&ca)),
            no_longer_dangling: in_a && dangling(&ca) && !(in_b && dangling(&cb)),
        });
    }
    out.sort_by(|x, y| x.contract.cmp(&y.contract));
    let (out, total) = cut(out, limit, partial);
    Ok((out, total))
}

/// Compare two snapshots: symbols (only for files that changed), links (`context_diff`
/// unchanged) and contracts.
pub fn semantic_diff(
    a: &impl Reader,
    b: &impl Reader,
    o: &DiffOptions,
) -> Result<SemanticDiff, StoreError> {
    let mut partial = Vec::new();
    let symbols = symbol_changes(a, b, o, &mut partial)?;
    let mut links = context_diff(a, b, o.limit)?;
    partial.append(&mut links.partial);
    let (contracts, contracts_total) = contract_changes(a, b, o.limit, &mut partial)?;
    Ok(SemanticDiff {
        symbols,
        links,
        contracts,
        contracts_total,
        partial,
    })
}
