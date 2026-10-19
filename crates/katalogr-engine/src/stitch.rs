//! Resolution of outgoing references against exports, and the reverse index that keeps
//! re-resolution to exactly the references that name a changed key (FEAT-001 section 4 and 6).

use crate::error::EngineError;
use crate::lexicon;
use katalogr_model::{
    ArtifactName, Confidence, Export, ExportKey, Fragment, Id, LinkKind, Name, Provenance,
};
use katalogr_store::{
    ArtifactRow, BindingRow, ContractRow, LinkEntry, LinkKey, LinkOrigin, LinkRow, Reader, RefId,
    Restitch, StoreError, SymbolRow, WriteTx,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

/// What one stitch call changed. Links that were removed and re-added unchanged cancel out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StitchDelta {
    pub links_added: Vec<LinkKey>,
    pub links_removed: Vec<LinkKey>,
    /// Every reference that was resolved (or re-resolved) by the call, sorted.
    pub refs_resolved: Vec<RefId>,
}

impl StitchDelta {
    pub fn merge(&mut self, other: StitchDelta) {
        self.links_added.extend(other.links_added);
        self.links_removed.extend(other.links_removed);
        self.refs_resolved.extend(other.refs_resolved);
    }
}

/// Stateless apart from the delta recorded by the [`Restitch`] entry points.
#[derive(Default)]
pub struct Stitcher {
    recorded: Mutex<StitchDelta>,
}

impl Stitcher {
    pub fn new() -> Stitcher {
        Stitcher::default()
    }

    /// The delta accumulated by `Restitch` calls since the last take.
    pub fn take_delta(&self) -> StitchDelta {
        std::mem::take(&mut *self.recorded.lock().expect("delta lock"))
    }

    /// Replace one artifact's fragment and re-resolve exactly the affected references:
    /// (a) this fragment's refs; (b) refs elsewhere whose candidates name an export key this
    /// fragment added or removed.
    pub fn replace_fragment(
        &self,
        tx: &mut WriteTx,
        f: Fragment,
    ) -> Result<StitchDelta, EngineError> {
        let mut cx = Cx::default();
        let aid = f.artifact.name.id();
        let old = tx.artifact(aid)?;
        if let Some(o) = &old
            && o.name != f.artifact.name
        {
            return Err(EngineError::IdCollision {
                id: aid,
                existing: o.name.canonical(),
                new: f.artifact.name.canonical(),
            });
        }
        let old_pairs = old.as_ref().map(export_pairs).unwrap_or_default();
        if let Some(o) = &old {
            lexicon::unindex_artifact(tx, o)?;
            purge(tx, &mut cx, aid, o)?;
        }

        let mut symbols = BTreeSet::new();
        for s in &f.symbols {
            let id = s.name.id();
            let new = s.name.canonical();
            if !symbols.insert(id) {
                return Err(EngineError::IdCollision {
                    id,
                    existing: new.clone(),
                    new,
                });
            }
            if let Some(row) = tx.symbol(id)? {
                return Err(EngineError::IdCollision {
                    id,
                    existing: row.name.canonical(),
                    new,
                });
            }
            tx.put_symbol(
                id,
                &SymbolRow {
                    name: s.name.clone(),
                    artifact: aid,
                    kind: s.kind.clone(),
                    span: s.span,
                    facts: s.facts.clone(),
                },
            )?;
        }

        let mut exports: Vec<Export> = f.exports.clone();
        // Every artifact is reachable by its file key, whichever extension indexed it (ADR-0016).
        exports.push(Export {
            key: file_key(&f.artifact.name),
            target: aid,
        });
        exports.sort_by(|a, b| (&a.key, a.target).cmp(&(&b.key, b.target)));
        exports.dedup();
        let row = ArtifactRow {
            name: f.artifact.name.clone(),
            extension: f.extension.clone(),
            provenance: f.provenance.clone(),
            facts: f.artifact.facts.clone(),
            symbols: symbols.into_iter().collect(),
            exports,
            refs: f.refs.clone(),
        };
        tx.put_artifact(aid, &row)?;
        lexicon::index_artifact(tx, &row)?;

        for l in &f.links {
            let key = LinkKey {
                from: l.from,
                kind: l.kind.clone(),
                to: l.to,
            };
            add_entry(
                tx,
                &mut cx,
                key,
                LinkEntry {
                    origin: LinkOrigin::Intra,
                    confidence: l.confidence,
                    span: l.span,
                    provenance: l.provenance.clone(),
                    fanout: None,
                    produced_by: None,
                },
            )?;
        }
        add_bindings(tx, &mut cx, &f, aid, &row)?;
        add_exports(tx, &row.exports)?;
        register_refs(tx, aid, &row)?;

        let new_pairs = export_pairs(&row);
        let mut changed: BTreeSet<ExportKey> = BTreeSet::new();
        for (k, _) in old_pairs.symmetric_difference(&new_pairs) {
            changed.insert(k.clone());
        }
        cx.rows.insert(aid, row.clone());

        let mut affected: BTreeSet<RefId> = (0..row.refs.len() as u32)
            .map(|index| RefId {
                artifact: aid,
                index,
            })
            .collect();
        for k in &changed {
            affected.extend(tx.refs_by_key(k)?);
        }
        for r in affected {
            resolve(tx, &mut cx, r)?;
        }
        Ok(cx.finish())
    }

    /// Remove an artifact and re-resolve the refs that named its exports.
    pub fn remove_artifact(
        &self,
        tx: &mut WriteTx,
        a: &ArtifactName,
    ) -> Result<StitchDelta, EngineError> {
        let mut cx = Cx::default();
        let aid = a.id();
        let Some(old) = tx.artifact(aid)? else {
            return Ok(StitchDelta::default());
        };
        lexicon::unindex_artifact(tx, &old)?;
        purge(tx, &mut cx, aid, &old)?;
        let mut affected: BTreeSet<RefId> = BTreeSet::new();
        for (k, _) in export_pairs(&old) {
            affected.extend(tx.refs_by_key(&k)?);
        }
        for r in affected {
            resolve(tx, &mut cx, r)?;
        }
        Ok(cx.finish())
    }
}

impl Restitch for Stitcher {
    fn replace_fragment(&self, tx: &mut WriteTx, f: Fragment) -> Result<(), StoreError> {
        let d = Stitcher::replace_fragment(self, tx, f).map_err(into_store)?;
        self.recorded.lock().expect("delta lock").merge(d);
        Ok(())
    }

    fn remove_artifact(&self, tx: &mut WriteTx, a: &ArtifactName) -> Result<(), StoreError> {
        let d = Stitcher::remove_artifact(self, tx, a).map_err(into_store)?;
        self.recorded.lock().expect("delta lock").merge(d);
        Ok(())
    }
}

fn into_store(e: EngineError) -> StoreError {
    match e {
        EngineError::Store(s) => s,
        other => StoreError::Stitch(other.to_string()),
    }
}

// ------------------------------------------------------------------ internals

#[derive(Default)]
struct Cx {
    added: BTreeSet<LinkKey>,
    removed: BTreeSet<LinkKey>,
    resolved: BTreeSet<RefId>,
    rows: BTreeMap<Id, ArtifactRow>,
}

impl Cx {
    fn finish(self) -> StitchDelta {
        StitchDelta {
            links_added: self.added.difference(&self.removed).cloned().collect(),
            links_removed: self.removed.difference(&self.added).cloned().collect(),
            refs_resolved: self.resolved.into_iter().collect(),
        }
    }
}

/// Namespace of the implicit file export every artifact carries.
pub const FILE_NS: &str = "file";

/// `("file", "<repo>:<path>")`, the key under which an artifact is always exported.
pub fn file_key(a: &ArtifactName) -> ExportKey {
    ExportKey {
        namespace: FILE_NS.to_string(),
        key: format!("{}:{}", a.repo, a.path),
    }
}

fn export_pairs(row: &ArtifactRow) -> BTreeSet<(ExportKey, Id)> {
    row.exports
        .iter()
        .map(|e| (e.key.clone(), e.target))
        .collect()
}

fn distinct_candidates(row: &ArtifactRow, index: usize) -> BTreeSet<&ExportKey> {
    row.refs[index].candidates.iter().collect()
}

/// Steps 1 and 2: drop everything the old version of the artifact contributed.
fn purge(tx: &mut WriteTx, cx: &mut Cx, aid: Id, old: &ArtifactRow) -> Result<(), EngineError> {
    for from in std::iter::once(aid).chain(old.symbols.iter().copied()) {
        for (key, _) in tx.links_out(from, None)? {
            tx.remove_link(&key)?;
            cx.removed.insert(key);
        }
    }
    for (r, _) in tx.unresolved_refs(Some(aid))? {
        tx.remove_unresolved(r)?;
    }
    for index in 0..old.refs.len() {
        for key in distinct_candidates(old, index) {
            let mut set = tx.refs_by_key(key)?;
            set.retain(|r| !(r.artifact == aid && r.index == index as u32));
            tx.put_refs_by_key(key, &set)?;
        }
    }
    for e in &old.exports {
        let mut targets = tx.exports(&e.key)?;
        targets.retain(|t| *t != e.target);
        tx.put_exports(&e.key, &targets)?;
    }
    withdraw_bindings(tx, aid)?;
    for s in &old.symbols {
        tx.remove_symbol(*s)?;
    }
    tx.remove_artifact_row(aid)?;
    cx.rows.remove(&aid);
    Ok(())
}

/// Undo the artifact's bindings: decrement the counts of their contracts and delete a contract
/// node when no binding names it any more. The binding links leave with the symbols' links.
fn withdraw_bindings(tx: &mut WriteTx, aid: Id) -> Result<(), EngineError> {
    let rows = tx.bindings_of(aid)?;
    for b in &rows {
        let Some(mut c) = tx.contract(b.contract)? else {
            continue;
        };
        c.adjust(b.role, -1);
        if c.is_empty() {
            tx.remove_contract(b.contract)?;
        } else {
            tx.put_contract(b.contract, &c)?;
        }
    }
    tx.put_bindings(aid, &[])?;
    Ok(())
}

/// Record the fragment's bindings: count them on their contract nodes (creating a node on first
/// use) and write `symbol --<role>--> contract` links. Duplicates count once. A binding whose
/// symbol is neither in the fragment nor the artifact, or whose contract kind is malformed,
/// is ignored.
fn add_bindings(
    tx: &mut WriteTx,
    cx: &mut Cx,
    f: &Fragment,
    aid: Id,
    row: &ArtifactRow,
) -> Result<(), EngineError> {
    let mut bindings: Vec<&katalogr_model::Binding> = f
        .bindings
        .iter()
        .filter(|b| {
            (b.symbol == aid || row.symbols.binary_search(&b.symbol).is_ok())
                && b.contract.validate().is_ok()
        })
        .collect();
    bindings.sort_by_key(|b| (b.contract.canonical(), b.role, b.symbol));
    bindings.dedup_by_key(|b| (b.contract.id(), b.role, b.symbol));
    let provenance: &Provenance = &row.provenance;
    let mut rows = Vec::with_capacity(bindings.len());
    for b in bindings {
        let cid = b.contract.id();
        let mut c = tx.contract(cid)?.unwrap_or_else(|| ContractRow {
            name: b.contract.clone(),
            providers: 0,
            consumers: 0,
            declarers: 0,
        });
        c.adjust(b.role, 1);
        tx.put_contract(cid, &c)?;
        add_entry(
            tx,
            cx,
            LinkKey {
                from: b.symbol,
                kind: LinkKind(b.role.as_str().to_string()),
                to: cid,
            },
            LinkEntry {
                origin: LinkOrigin::Intra,
                confidence: b.confidence,
                span: b.span,
                provenance: provenance.clone(),
                fanout: None,
                produced_by: b.origin.clone(),
            },
        )?;
        rows.push(BindingRow {
            symbol: b.symbol,
            contract: cid,
            role: b.role,
        });
    }
    tx.put_bindings(aid, &rows)?;
    Ok(())
}

fn add_exports(tx: &mut WriteTx, exports: &[Export]) -> Result<(), EngineError> {
    for e in exports {
        let mut targets = tx.exports(&e.key)?;
        if let Err(pos) = targets.binary_search(&e.target) {
            targets.insert(pos, e.target);
            tx.put_exports(&e.key, &targets)?;
        }
    }
    Ok(())
}

fn register_refs(tx: &mut WriteTx, aid: Id, row: &ArtifactRow) -> Result<(), EngineError> {
    for index in 0..row.refs.len() {
        let me = RefId {
            artifact: aid,
            index: index as u32,
        };
        for key in distinct_candidates(row, index) {
            let mut set = tx.refs_by_key(key)?;
            if let Err(pos) = set.binary_search(&me) {
                set.insert(pos, me);
                tx.put_refs_by_key(key, &set)?;
            }
        }
    }
    Ok(())
}

fn add_entry(
    tx: &mut WriteTx,
    cx: &mut Cx,
    key: LinkKey,
    entry: LinkEntry,
) -> Result<(), EngineError> {
    let mut row = match tx.link(&key)? {
        Some(r) => r,
        None => {
            cx.added.insert(key.clone());
            LinkRow {
                entries: Vec::new(),
            }
        }
    };
    row.entries.retain(|e| e.origin != entry.origin);
    row.entries.push(entry);
    row.entries.sort_by_key(|e| e.origin);
    tx.put_link(&key, &row)?;
    Ok(())
}

/// Drop the entries of `origin` from the `(from, kind)` links; a row left empty is removed.
fn drop_origin(
    tx: &mut WriteTx,
    cx: &mut Cx,
    from: Id,
    kind: &LinkKind,
    origin: LinkOrigin,
) -> Result<(), EngineError> {
    for (key, mut row) in tx.links_out(from, Some(kind))? {
        let before = row.entries.len();
        row.entries.retain(|e| e.origin != origin);
        if row.entries.len() == before {
            continue;
        }
        if row.entries.is_empty() {
            tx.remove_link(&key)?;
            cx.removed.insert(key);
        } else {
            tx.put_link(&key, &row)?;
        }
    }
    Ok(())
}

/// Resolution of one reference (FEAT-001 section 6): the first candidate with exports wins; one
/// target keeps the ref's confidence, several give one `Inferred` link each, none is recorded
/// in `refs_unresolved`.
fn resolve(tx: &mut WriteTx, cx: &mut Cx, rid: RefId) -> Result<(), EngineError> {
    if let std::collections::btree_map::Entry::Vacant(slot) = cx.rows.entry(rid.artifact) {
        let Some(row) = tx.artifact(rid.artifact)? else {
            return Ok(());
        };
        slot.insert(row);
    }
    let row = &cx.rows[&rid.artifact];
    let Some(r) = row.refs.get(rid.index as usize).cloned() else {
        return Ok(());
    };
    let provenance = row.provenance.clone();

    drop_origin(tx, cx, r.from, &r.kind, LinkOrigin::Ref(rid.index))?;
    tx.remove_unresolved(rid)?;

    let mut targets = Vec::new();
    for cand in &r.candidates {
        targets = tx.exports(cand)?;
        if !targets.is_empty() {
            break;
        }
    }
    let confidence = if targets.len() == 1 {
        r.confidence
    } else {
        Confidence::Inferred
    };
    let fanout = u32::try_from(targets.len()).unwrap_or(u32::MAX);
    if targets.is_empty() {
        tx.put_unresolved(rid, &r)?;
    }
    for to in targets {
        add_entry(
            tx,
            cx,
            LinkKey {
                from: r.from,
                kind: r.kind.clone(),
                to,
            },
            LinkEntry {
                origin: LinkOrigin::Ref(rid.index),
                confidence,
                span: r.span,
                provenance: provenance.clone(),
                fanout: (confidence == Confidence::Inferred).then_some(fanout),
                produced_by: r.origin.clone(),
            },
        )?;
    }
    cx.resolved.insert(rid);
    Ok(())
}
