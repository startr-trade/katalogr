//! Graph diff and conflict detection between contexts (FEAT-043 R-8, R-9).
//!
//! Both are pure over readers. Names, never ids, appear in the output, and every list is sorted.

use crate::metrics::Better;
use katalogr_model::{Id, Name};
use katalogr_query::result::{DanglingRef, Partial, cap};
use katalogr_store::{LinkKey, Reader, StoreError};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// One link, by the canonical names of its ends.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
pub struct LinkDelta {
    pub from: String,
    pub kind: String,
    pub to: String,
}

/// One measure in both contexts, as `graph.symbols`, `graph.links` and `graph.dangling`.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct MetricDelta {
    pub name: String,
    pub a: f64,
    pub b: f64,
    /// `b - a`.
    pub delta: f64,
    pub better: Better,
}

/// What differs between context `a` and context `b`. A list cut by the limit adds one
/// `limit` entry to `partial`, in the order `links_added`, `links_removed`, `dangling_added`,
/// `dangling_removed`.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ContextDiff {
    pub links_added: Vec<LinkDelta>,
    pub links_removed: Vec<LinkDelta>,
    pub dangling_added: Vec<DanglingRef>,
    pub dangling_removed: Vec<DanglingRef>,
    pub metrics: Vec<MetricDelta>,
    #[serde(skip)]
    #[schemars(skip)]
    pub partial: Vec<Partial>,
}

/// Contracts each context changed against the base, and those both changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ContextConflicts {
    pub changed_a: Vec<String>,
    pub changed_b: Vec<String>,
    /// The sorted intersection of the two.
    pub conflicts: Vec<String>,
}

fn name_of(id: Id, readers: &[&dyn NameLookup]) -> String {
    readers
        .iter()
        .find_map(|r| r.name(id))
        .unwrap_or_else(|| id.to_hex())
}

trait NameLookup {
    fn name(&self, id: Id) -> Option<String>;
}

impl<R: Reader> NameLookup for R {
    fn name(&self, id: Id) -> Option<String> {
        if let Ok(Some(s)) = self.symbol(id) {
            return Some(s.name.canonical());
        }
        if let Ok(Some(a)) = self.artifact(id) {
            return Some(a.name.canonical());
        }
        if let Ok(Some(c)) = self.contract(id) {
            return Some(c.name.canonical());
        }
        None
    }
}

fn cut<T>(mut v: Vec<T>, limit: usize, partial: &mut Vec<Partial>) -> Vec<T> {
    if v.len() > limit {
        partial.push(Partial::new(
            cap::LIMIT,
            limit as u64,
            limit as u64,
            Some(v.len() as u64),
        ));
        v.truncate(limit);
    }
    v
}

fn dangling_all(
    tx: &impl Reader,
) -> Result<BTreeMap<(String, String, Vec<String>), DanglingRef>, StoreError> {
    let mut kinds: Vec<&str> = katalogr_query::DANGLING_KINDS.to_vec();
    kinds.push(katalogr_query::DANGLING_CONTRACT);
    Ok(katalogr_query::dangling(tx, &kinds)?
        .into_iter()
        .map(|d| ((d.from.name.clone(), d.kind.clone(), d.wanted.clone()), d))
        .collect())
}

/// Compare two contexts' graphs. Pure over two readers, output sorted, capped at `limit` per
/// list. `links_all` of each reader is sorted by key, so one merge join finds both sides.
pub fn context_diff(
    a: &impl Reader,
    b: &impl Reader,
    limit: usize,
) -> Result<ContextDiff, StoreError> {
    let la: Vec<LinkKey> = a.links_all()?.into_iter().map(|(k, _)| k).collect();
    let lb: Vec<LinkKey> = b.links_all()?.into_iter().map(|(k, _)| k).collect();
    let (mut added, mut removed) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < la.len() || j < lb.len() {
        match (la.get(i), lb.get(j)) {
            (Some(x), Some(y)) if x == y => {
                i += 1;
                j += 1;
            }
            (Some(x), Some(y)) if x < y => {
                removed.push(x.clone());
                i += 1;
            }
            (Some(_), Some(y)) => {
                added.push(y.clone());
                j += 1;
            }
            (Some(x), None) => {
                removed.push(x.clone());
                i += 1;
            }
            (None, Some(y)) => {
                added.push(y.clone());
                j += 1;
            }
            (None, None) => break,
        }
    }
    let delta = |k: &LinkKey| LinkDelta {
        from: name_of(k.from, &[a, b]),
        kind: k.kind.0.clone(),
        to: name_of(k.to, &[a, b]),
    };
    let mut links_added: Vec<LinkDelta> = added.iter().map(delta).collect();
    let mut links_removed: Vec<LinkDelta> = removed.iter().map(delta).collect();
    links_added.sort();
    links_removed.sort();

    let (da, db) = (dangling_all(a)?, dangling_all(b)?);
    let dangling_added: Vec<DanglingRef> = db
        .iter()
        .filter(|(k, _)| !da.contains_key(*k))
        .map(|(_, v)| v.clone())
        .collect();
    let dangling_removed: Vec<DanglingRef> = da
        .iter()
        .filter(|(k, _)| !db.contains_key(*k))
        .map(|(_, v)| v.clone())
        .collect();

    let metric = |name: &str, x: usize, y: usize, better: Better| MetricDelta {
        name: name.to_string(),
        a: x as f64,
        b: y as f64,
        delta: y as f64 - x as f64,
        better,
    };
    let metrics = vec![
        metric("graph.dangling", da.len(), db.len(), Better::Lower),
        metric("graph.links", la.len(), lb.len(), Better::Info),
        metric(
            "graph.symbols",
            a.symbols()?.len(),
            b.symbols()?.len(),
            Better::Info,
        ),
    ];

    let mut partial = Vec::new();
    Ok(ContextDiff {
        links_added: cut(links_added, limit, &mut partial),
        links_removed: cut(links_removed, limit, &mut partial),
        dangling_added: cut(dangling_added, limit, &mut partial),
        dangling_removed: cut(dangling_removed, limit, &mut partial),
        metrics,
        partial,
    })
}

/// A contract as the graph holds it: its counts and every binding that names it.
fn contract_shapes(
    tx: &impl Reader,
) -> Result<BTreeMap<Id, (String, BTreeSet<(Id, String)>)>, StoreError> {
    let mut out: BTreeMap<Id, (String, BTreeSet<(Id, String)>)> = BTreeMap::new();
    for (id, row) in tx.contracts()? {
        let shape = format!("{}/{}/{}", row.providers, row.consumers, row.declarers);
        out.insert(id, (shape, BTreeSet::new()));
    }
    for (aid, _) in tx.artifacts()? {
        for bnd in tx.bindings_of(aid)? {
            if let Some(e) = out.get_mut(&bnd.contract) {
                e.1.insert((bnd.symbol, format!("{:?}", bnd.role)));
            }
        }
    }
    Ok(out)
}

/// Contracts each of `a` and `b` changed against `base`, and their intersection.
pub fn context_conflicts(
    base: &impl Reader,
    a: &impl Reader,
    b: &impl Reader,
) -> Result<ContextConflicts, StoreError> {
    let sb = contract_shapes(base)?;
    fn changed(
        sb: &BTreeMap<Id, (String, BTreeSet<(Id, String)>)>,
        base: &impl Reader,
        other: &impl Reader,
    ) -> Result<BTreeSet<String>, StoreError> {
        let so = contract_shapes(other)?;
        let mut out = BTreeSet::new();
        for id in sb.keys().chain(so.keys()) {
            if sb.get(id) != so.get(id) {
                out.insert(name_of(*id, &[other, base]));
            }
        }
        Ok(out)
    }
    let (ca, cb) = (changed(&sb, base, a)?, changed(&sb, base, b)?);
    let conflicts = ca.intersection(&cb).cloned().collect();
    Ok(ContextConflicts {
        changed_a: ca.into_iter().collect(),
        changed_b: cb.into_iter().collect(),
        conflicts,
    })
}
