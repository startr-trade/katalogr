//! `mirror_drift`: copies of one contract compared field by field (FEAT-009 section 6.5).
//!
//! A contract with two or more `declares` bindings is a mirror set: the same structure written
//! twice, for example a schema kept in two repositories or in two formats. The structural
//! facts compared are the neutral names defined in `katalogr_model::structure`.

use crate::resolve::hit;
use crate::result::{FieldDiff, FieldShape, MirrorDrift, SymbolHit};
use katalogr_model::structure::{FIELD_TYPE, MANY, REQUIRED};
use katalogr_model::{Fact, Id, LinkKind, Name};
use katalogr_store::{Reader, StoreError, SymbolBrief, SymbolRow};
use std::collections::BTreeMap;

/// How deep below a declarer fields are looked for; only stops cycles through hand-made
/// `contains` loops.
const MAX_FIELD_DEPTH: usize = 16;

/// For every contract with two or more `declares` bindings (only `kinds` when non-empty),
/// compare each declarer against the first by name. Pairs with no difference are omitted;
/// incomparable pairs are kept with `comparable: false`. Sorted by (contract, right name).
///
/// The reads are batched (all contracts, then all declarers, then one level of fields at a
/// time for all declarers), so the cost is a few table lookups plus the rows read.
pub fn mirror_drift(tx: &impl Reader, kinds: &[&str]) -> Result<Vec<MirrorDrift>, StoreError> {
    let mut contracts: Vec<(String, Id)> = tx
        .contracts()?
        .into_iter()
        .filter(|(_, row)| {
            row.declarers >= 2 && (kinds.is_empty() || kinds.contains(&row.name.kind.as_str()))
        })
        .map(|(id, row)| (row.name.canonical(), id))
        .collect();
    contracts.sort();
    let declares = LinkKind("declares".to_string());

    // The declarers of each contract, sorted by name.
    let ids: Vec<Id> = contracts.iter().map(|(_, id)| *id).collect();
    let mut declarer_ids: Vec<Vec<Id>> = Vec::with_capacity(ids.len());
    for keys in tx.links_in_many(&ids)? {
        declarer_ids.push(
            keys.into_iter()
                .filter(|k| k.kind == declares)
                .map(|k| k.from)
                .collect(),
        );
    }
    let flat: Vec<Id> = declarer_ids.iter().flatten().copied().collect();
    let mut rows = tx.symbols_many(&flat)?.into_iter();
    let mut groups: Vec<Vec<SymbolRow>> = Vec::with_capacity(contracts.len());
    for ids in &declarer_ids {
        let mut g: Vec<SymbolRow> = Vec::with_capacity(ids.len());
        for _ in ids {
            g.extend(rows.next().flatten());
        }
        g.sort_by_cached_key(|r| r.name.canonical());
        groups.push(g);
    }

    // The fields of every declarer of a contract that still has two or more.
    let wanted: Vec<&SymbolRow> = groups
        .iter()
        .filter(|g| g.len() >= 2)
        .flat_map(|g| g.iter())
        .collect();
    let mut fields = fields_of_all(tx, &wanted)?.into_iter();

    let mut out = Vec::new();
    for ((name, _), group) in contracts.iter().zip(&groups) {
        if group.len() < 2 {
            continue;
        }
        let left_hit = hit(&group[0]);
        let left_fields = fields.next().unwrap_or_default();
        for right in &group[1..] {
            let right_fields = fields.next().unwrap_or_default();
            let comparable = !left_fields.is_empty() && !right_fields.is_empty();
            let differences = if comparable {
                compare(&left_fields, &right_fields)
            } else {
                Vec::new()
            };
            if comparable && differences.is_empty() {
                continue;
            }
            out.push(MirrorDrift {
                contract: name.clone(),
                left: left_hit.clone(),
                right: hit(right) as SymbolHit,
                comparable,
                differences,
            });
        }
    }
    out.sort_by(|a, b| (&a.contract, &a.right.name).cmp(&(&b.contract, &b.right.name)));
    Ok(out)
}

/// A path segment compared across copies: lowercase, without a leading `@`, `_` or `-`.
fn normal(segment: &str) -> String {
    if segment
        .bytes()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return segment.to_string();
    }
    segment
        .trim_start_matches('@')
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

fn shape(row: &SymbolBrief) -> Option<FieldShape> {
    let Some(Fact::Str(ty)) = row.facts.get(FIELD_TYPE) else {
        return None;
    };
    let flag = |k: &str| match row.facts.get(k) {
        Some(Fact::Bool(b)) => Some(*b),
        _ => None,
    };
    Some(FieldShape {
        field_type: Some(
            ty.rsplit_once('}')
                .map_or(ty.as_str(), |(_, l)| l)
                .to_lowercase(),
        ),
        required: flag(REQUIRED),
        many: flag(MANY),
    })
}

/// For each root, every symbol below it that has a `field_type` fact, by its normalised path.
/// Walks `contains` links breadth first, one level for all roots at a time.
fn fields_of_all(
    tx: &impl Reader,
    roots: &[&SymbolRow],
) -> Result<Vec<BTreeMap<String, FieldShape>>, StoreError> {
    let contains = LinkKind("contains".to_string());
    let mut out: Vec<BTreeMap<String, FieldShape>> = vec![BTreeMap::new(); roots.len()];
    // (root index, symbol, normalised path so far)
    let mut frontier: Vec<(usize, Id, String)> = roots
        .iter()
        .enumerate()
        .map(|(i, r)| (i, r.name.id(), String::new()))
        .collect();
    for _ in 0..MAX_FIELD_DEPTH {
        if frontier.is_empty() {
            break;
        }
        let from: Vec<Id> = frontier.iter().map(|(_, id, _)| *id).collect();
        let mut children: Vec<(usize, Id, String)> = Vec::new();
        for ((root, _, path), keys) in frontier
            .iter()
            .zip(tx.link_keys_out_many(&from, &contains)?)
        {
            for k in keys {
                children.push((*root, k.to, path.clone()));
            }
        }
        let ids: Vec<Id> = children.iter().map(|(_, id, _)| *id).collect();
        let mut next = Vec::new();
        for ((root, id, path), row) in children.into_iter().zip(tx.symbol_briefs_many(&ids)?) {
            let Some(child) = row else { continue };
            let seg = normal(&child.leaf);
            let p = if path.is_empty() {
                seg
            } else {
                format!("{path}/{seg}")
            };
            let nested = match shape(&child) {
                Some(s) => {
                    // Fields nest under a field of type `object`; any other field is a leaf,
                    // so its links are not read.
                    let object = s.field_type.as_deref() == Some("object");
                    out[root].entry(p.clone()).or_insert(s);
                    object
                }
                // Not a field: a container (a group, say) that fields may sit below.
                None => true,
            };
            if nested {
                next.push((root, id, p));
            }
        }
        frontier = next;
    }
    Ok(out)
}

fn compare(
    left: &BTreeMap<String, FieldShape>,
    right: &BTreeMap<String, FieldShape>,
) -> Vec<FieldDiff> {
    let mut diffs = Vec::new();
    let mut diff = |field: &str, what: &str, l: Option<&FieldShape>, r: Option<&FieldShape>| {
        diffs.push(FieldDiff {
            field: field.to_string(),
            what: what.to_string(),
            left: l.cloned(),
            right: r.cloned(),
        });
    };
    for (field, l) in left {
        match right.get(field) {
            None => diff(field, "missing-right", Some(l), None),
            Some(r) => {
                if matches!((&l.field_type, &r.field_type), (Some(a), Some(b)) if a != b) {
                    diff(field, "type", Some(l), Some(r));
                }
                if matches!((l.required, r.required), (Some(a), Some(b)) if a != b) {
                    diff(field, "required", Some(l), Some(r));
                }
                if matches!((l.many, r.many), (Some(a), Some(b)) if a != b) {
                    diff(field, "many", Some(l), Some(r));
                }
            }
        }
    }
    for (field, r) in right {
        if !left.contains_key(field) {
            diff(field, "missing-left", None, Some(r));
        }
    }
    diffs.sort_by(|a, b| (&a.field, &a.what).cmp(&(&b.field, &b.what)));
    diffs
}
