//! Resolution of outgoing references against exports, and the reverse index that keeps
//! re-resolution to exactly the references that name a changed key (FEAT-001 section 4 and 6).

use crate::error::EngineError;
use crate::lexicon;
use katalogr_model::{
    ArtifactName, Binding, Confidence, ContractName, ContractRole, Export, ExportKey, Fragment, Id,
    Link, LinkKind, Name, OutRef, Provenance, RefBinding,
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

/// Everything `replace_fragment` derives from a fragment alone, computed without the cache
/// (see [`Stitcher::prepare`]).
pub struct PreparedFragment {
    pub(crate) aid: Id,
    pub(crate) name: ArtifactName,
    /// Symbol ids in fragment order with their rows, up to the first id repeated within the
    /// fragment.
    pub(crate) symbols: Vec<(Id, SymbolRow)>,
    /// The first id repeated within the fragment and its canonical name; `apply` raises it
    /// after the symbols before it.
    pub(crate) dup: Option<(Id, String)>,
    /// Exports sorted and deduplicated, with the file key.
    pub(crate) row: ArtifactRow,
    pub(crate) pairs: BTreeSet<(ExportKey, Id)>,
    pub(crate) links: Vec<Link>,
    /// Plain bindings, filtered, sorted and deduplicated.
    pub(crate) bindings: Vec<Binding>,
    /// Bound references that count as a binding of their `from` symbol, in reference order.
    pub(crate) bound_refs: Vec<(Id, ContractName, ContractRole)>,
    /// Lexicon documents in `row.symbols` order.
    pub(crate) docs: Vec<(Id, lexicon::Doc)>,
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
        self.apply(tx, Self::prepare(f)?)
    }

    /// Pure: everything `replace_fragment` derives from a fragment alone, without the cache,
    /// so it is safe on any thread. Never fails today; it returns `Result` so a later check can
    /// refuse a fragment before the writer sees it.
    pub fn prepare(f: Fragment) -> Result<PreparedFragment, EngineError> {
        let aid = f.artifact.name.id();
        let mut ids = BTreeSet::new();
        let mut symbols = Vec::with_capacity(f.symbols.len());
        let mut dup = None;
        for s in &f.symbols {
            let id = s.name.id();
            if !ids.insert(id) {
                dup = Some((id, s.name.canonical()));
                break;
            }
            symbols.push((
                id,
                SymbolRow {
                    name: s.name.clone(),
                    artifact: aid,
                    kind: s.kind.clone(),
                    span: s.span,
                    facts: s.facts.clone(),
                },
            ));
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
            symbols: ids.into_iter().collect(),
            exports,
            refs: f.refs.clone(),
        };
        let pairs = export_pairs(&row);

        let mut bindings: Vec<Binding> = f
            .bindings
            .iter()
            .filter(|b| {
                (b.symbol == aid || row.symbols.binary_search(&b.symbol).is_ok())
                    && b.contract.validate().is_ok()
            })
            .cloned()
            .collect();
        bindings.sort_by_key(|b| (b.contract.canonical(), b.role, b.symbol));
        bindings.dedup_by_key(|b| (b.contract.id(), b.role, b.symbol));
        // A bound reference counts like a binding of the registering symbol; its links are
        // written when it resolves. Duplicates of a plain binding or of another reference
        // count once.
        let mut held: BTreeSet<(Id, ContractRole, Id)> = bindings
            .iter()
            .map(|b| (b.contract.id(), b.role, b.symbol))
            .collect();
        let mut bound_refs = Vec::new();
        for r in &f.refs {
            let Some(b) = &r.binds else { continue };
            if !(r.from == aid || row.symbols.binary_search(&r.from).is_ok())
                || b.contract.validate().is_err()
                || !held.insert((b.contract.id(), b.role, r.from))
            {
                continue;
            }
            bound_refs.push((r.from, b.contract.clone(), b.role));
        }
        let docs = lexicon::docs_of_artifact(&row, &symbols);
        Ok(PreparedFragment {
            aid,
            name: f.artifact.name,
            symbols,
            dup,
            row,
            pairs,
            links: f.links,
            bindings,
            bound_refs,
            docs,
        })
    }

    /// The writer half: the exact sequence of cache reads and writes `replace_fragment`
    /// performed before the split, fed from `p`. Same errors, same order, same delta.
    pub fn apply(&self, tx: &mut WriteTx, p: PreparedFragment) -> Result<StitchDelta, EngineError> {
        let mut cx = Cx::default();
        let PreparedFragment {
            aid,
            name,
            symbols,
            dup,
            row,
            pairs: new_pairs,
            links,
            bindings,
            bound_refs,
            docs,
        } = p;
        let old = tx.artifact(aid)?;
        if let Some(o) = &old
            && o.name != name
        {
            return Err(EngineError::IdCollision {
                id: aid,
                existing: o.name.canonical(),
                new: name.canonical(),
            });
        }
        let old_pairs = old.as_ref().map(export_pairs).unwrap_or_default();
        if let Some(o) = &old {
            lexicon::unindex_artifact(tx, o)?;
            purge(tx, &mut cx, aid, o)?;
        }

        for (id, srow) in &symbols {
            if let Some(existing) = tx.symbol(*id)? {
                return Err(EngineError::IdCollision {
                    id: *id,
                    existing: existing.name.canonical(),
                    new: srow.name.canonical(),
                });
            }
            tx.put_symbol(*id, srow)?;
        }
        if let Some((id, canonical)) = dup {
            return Err(EngineError::IdCollision {
                id,
                existing: canonical.clone(),
                new: canonical,
            });
        }

        tx.put_artifact(aid, &row)?;
        lexicon::index_docs(tx, &docs)?;

        for l in &links {
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
        add_bindings(tx, &mut cx, aid, &row, &bindings, &bound_refs)?;
        add_exports(tx, &row.exports)?;
        register_refs(tx, aid, &row)?;
        cx.fresh = (0..row.refs.len() as u32)
            .map(|index| RefId {
                artifact: aid,
                index,
            })
            .collect();
        cx.fresh_from = std::iter::once(aid)
            .chain(row.symbols.iter().copied())
            .collect();

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
    /// References of the artifact `apply` just purged and re-registered that `resolve` has not
    /// yet seen (FEAT-072 R-10): the purge already removed the link entries they would drop.
    fresh: BTreeSet<RefId>,
    /// The artifact and its symbols: the only `from` nodes the purge cleared of `Ref` entries.
    fresh_from: BTreeSet<Id>,
}

#[cfg(test)]
thread_local! {
    /// Resolutions that skipped their origin scan (R-10).
    static FRESH_SKIPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
    // Links this artifact's bound references wrote leave first, wherever they start (ADR-0022).
    for (index, r) in old.refs.iter().enumerate() {
        if let Some(b) = &r.binds {
            drop_bound(
                tx,
                cx,
                b.contract.id(),
                b.role,
                LinkOrigin::Bound {
                    artifact: aid,
                    index: index as u32,
                },
            )?;
        }
    }
    // Then everything else the artifact's nodes start, except another artifact's bound links
    // held by this artifact's symbols: re-resolution moves those when the symbol goes.
    for from in std::iter::once(aid).chain(old.symbols.iter().copied()) {
        for (key, mut row) in tx.links_out(from, None)? {
            row.entries.retain(
                |e| matches!(e.origin, LinkOrigin::Bound { artifact, .. } if artifact != aid),
            );
            if row.entries.is_empty() {
                tx.remove_link(&key)?;
                cx.removed.insert(key);
            } else {
                tx.put_link(&key, &row)?;
            }
        }
    }
    for (r, _) in tx.unresolved_refs(Some(aid))? {
        tx.remove_unresolved(r)?;
    }
    for index in 0..old.refs.len() {
        for key in distinct_candidates(old, index) {
            tx.remove_ref_by_key(
                key,
                RefId {
                    artifact: aid,
                    index: index as u32,
                },
            )?;
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
/// was dropped by `prepare`.
fn add_bindings(
    tx: &mut WriteTx,
    cx: &mut Cx,
    aid: Id,
    row: &ArtifactRow,
    bindings: &[Binding],
    bound_refs: &[(Id, ContractName, ContractRole)],
) -> Result<(), EngineError> {
    let provenance: &Provenance = &row.provenance;
    let mut rows = Vec::with_capacity(bindings.len());
    for (from, contract, role) in bound_refs {
        let cid = contract.id();
        let mut c = tx.contract(cid)?.unwrap_or_else(|| ContractRow {
            name: contract.clone(),
            providers: 0,
            consumers: 0,
            declarers: 0,
        });
        c.adjust(*role, 1);
        tx.put_contract(cid, &c)?;
        rows.push(BindingRow {
            symbol: *from,
            contract: cid,
            role: *role,
        });
    }
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
            tx.add_ref_by_key(key, me)?;
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
) -> Result<usize, EngineError> {
    let mut changed = 0;
    for (key, mut row) in tx.links_out(from, Some(kind))? {
        let before = row.entries.len();
        row.entries.retain(|e| e.origin != origin);
        if row.entries.len() == before {
            continue;
        }
        changed += 1;
        if row.entries.is_empty() {
            tx.remove_link(&key)?;
            cx.removed.insert(key);
        } else {
            tx.put_link(&key, &row)?;
        }
    }
    Ok(changed)
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
    // R-10: a reference resolved for the first time right after its artifact was purged and
    // re-registered has no entries left to drop.
    let fresh = cx.fresh.remove(&rid) && cx.fresh_from.contains(&r.from);

    if let Some(b) = &r.binds {
        return resolve_bound(tx, cx, rid, &r, b, provenance, fresh);
    }
    if fresh {
        #[cfg(test)]
        FRESH_SKIPS.with(|c| c.set(c.get() + 1));
        #[cfg(debug_assertions)]
        {
            let n = drop_origin(tx, cx, r.from, &r.kind, LinkOrigin::Ref(rid.index))?;
            debug_assert_eq!(n, 0, "fresh reference {rid:?} had origin entries to drop");
        }
    } else {
        drop_origin(tx, cx, r.from, &r.kind, LinkOrigin::Ref(rid.index))?;
    }
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

/// Remove the links of `role` into `contract` that carry `origin`; a row left empty is removed.
fn drop_bound(
    tx: &mut WriteTx,
    cx: &mut Cx,
    contract: Id,
    role: ContractRole,
    origin: LinkOrigin,
) -> Result<usize, EngineError> {
    let mut changed = 0;
    let kind = LinkKind(role.as_str().to_string());
    for key in tx.links_in(contract)? {
        if key.kind != kind {
            continue;
        }
        let Some(mut row) = tx.link(&key)? else {
            continue;
        };
        let before = row.entries.len();
        row.entries.retain(|e| e.origin != origin);
        if row.entries.len() == before {
            continue;
        }
        changed += 1;
        if row.entries.is_empty() {
            tx.remove_link(&key)?;
            cx.removed.insert(key);
        } else {
            tx.put_link(&key, &row)?;
        }
    }
    Ok(changed)
}

/// Resolution of a reference that holds a contract binding (ADR-0022): every resolved target
/// holds `target --role--> contract`; while nothing resolves, the reference's `from` symbol holds
/// it, inferred, and the reference is listed as unresolved.
fn resolve_bound(
    tx: &mut WriteTx,
    cx: &mut Cx,
    rid: RefId,
    r: &OutRef,
    b: &RefBinding,
    provenance: Provenance,
    fresh: bool,
) -> Result<(), EngineError> {
    let (contract, role) = (b.contract.id(), b.role);
    let origin = LinkOrigin::Bound {
        artifact: rid.artifact,
        index: rid.index,
    };
    if fresh {
        #[cfg(test)]
        FRESH_SKIPS.with(|c| c.set(c.get() + 1));
        #[cfg(debug_assertions)]
        {
            let n = drop_bound(tx, cx, contract, role, origin)?;
            debug_assert_eq!(n, 0, "fresh bound reference {rid:?} had links to drop");
        }
    } else {
        drop_bound(tx, cx, contract, role, origin)?;
    }
    tx.remove_unresolved(rid)?;
    let mut targets = Vec::new();
    for cand in &r.candidates {
        targets = tx.exports(cand)?;
        if !targets.is_empty() {
            break;
        }
    }
    let kind = LinkKind(role.as_str().to_string());
    let fallback = targets.is_empty();
    let (holders, confidence) = if fallback {
        tx.put_unresolved(rid, r)?;
        (vec![r.from], Confidence::Inferred)
    } else if targets.len() == 1 {
        (targets, r.confidence)
    } else {
        (targets, Confidence::Inferred)
    };
    let fanout = u32::try_from(holders.len()).unwrap_or(u32::MAX);
    for from in holders {
        add_entry(
            tx,
            cx,
            LinkKey {
                from,
                kind: kind.clone(),
                to: contract,
            },
            LinkEntry {
                origin,
                confidence,
                span: r.span,
                provenance: provenance.clone(),
                fanout: (!fallback && confidence == Confidence::Inferred).then_some(fanout),
                produced_by: r.origin.clone(),
            },
        )?;
    }
    cx.resolved.insert(rid);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use katalogr_model::{ArtifactNode, RelPath, Segment, Span, SymbolName, SymbolNode};
    use katalogr_store::{Cache, Kindex, ProjectConfig, ProjectName, RepoConfig};
    use std::time::{Duration, Instant};

    fn cache(t: &tempfile::TempDir) -> (Cache, Stitcher) {
        let root = Utf8PathBuf::from_path_buf(t.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("p").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let stitcher = Stitcher::new();
        let c = Cache::open_or_rebuild(&dir, &stitcher, Instant::now() + Duration::from_secs(5))
            .unwrap();
        (c, stitcher)
    }

    fn key(ns: &str, k: &str) -> ExportKey {
        ExportKey {
            namespace: ns.into(),
            key: k.into(),
        }
    }

    /// A fragment of `path` with one function symbol per name; `exports` are `(key, symbol
    /// index)`, `refs` are `(from symbol index, candidate key)`.
    fn frag(
        path: &str,
        names: &[&str],
        exports: &[(&str, usize)],
        refs: &[(usize, &str)],
    ) -> Fragment {
        let artifact = ArtifactName::new("r", path).unwrap();
        let symbols: Vec<SymbolNode> = names
            .iter()
            .map(|n| SymbolNode {
                name: SymbolName {
                    artifact: artifact.clone(),
                    segments: vec![Segment {
                        kind: "fn".into(),
                        name: (*n).into(),
                    }],
                    ordinal: 1,
                },
                kind: "fn".into(),
                span: Span {
                    start_line: 1,
                    start_col: 1,
                    end_line: 1,
                    end_col: 1,
                },
                facts: BTreeMap::new(),
            })
            .collect();
        let id = |i: usize| symbols[i].name.id();
        Fragment {
            provenance: Provenance {
                repo: "r".into(),
                path: RelPath::new(path).unwrap(),
                revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
            },
            extension: "test".into(),
            artifact: ArtifactNode {
                name: artifact,
                facts: BTreeMap::new(),
            },
            links: Vec::new(),
            refs: refs
                .iter()
                .map(|(from, cand)| OutRef {
                    from: id(*from),
                    kind: LinkKind("calls".into()),
                    span: None,
                    candidates: vec![key("ns", cand)],
                    confidence: Confidence::Extracted,
                    origin: None,
                    binds: None,
                })
                .collect(),
            exports: exports
                .iter()
                .map(|(k, i)| Export {
                    key: key("ns", k),
                    target: id(*i),
                })
                .collect(),
            bindings: Vec::new(),
            symbols,
        }
    }

    fn sequence() -> Vec<Fragment> {
        vec![
            frag("a.tx", &["A", "A2"], &[("k", 0), ("z", 1)], &[]),
            frag("b.tx", &["B"], &[], &[(0, "k"), (0, "missing")]),
            frag("c.tx", &["C"], &[("k", 0)], &[(0, "z")]),
            // Re-replace with changed refs and exports.
            frag(
                "b.tx",
                &["B", "B2"],
                &[("missing", 1)],
                &[(0, "k"), (1, "z")],
            ),
            frag("a.tx", &["A"], &[("k", 0)], &[]),
        ]
    }

    #[test]
    fn fresh_resolution_skips_drop_origin() {
        let t = tempfile::tempdir().unwrap();
        let (c, s) = cache(&t);
        let put = |f: Fragment| {
            FRESH_SKIPS.with(|n| n.set(0));
            let mut tx = c.write().unwrap();
            s.replace_fragment(&mut tx, f).unwrap();
            tx.commit().unwrap();
            FRESH_SKIPS.with(|n| n.get())
        };
        put(frag("a.tx", &["A"], &[("k", 0)], &[]));
        // Three references of the artifact being written, resolved right after the purge and
        // re-registration: none scans for old entries.
        let b = frag("b.tx", &["B"], &[], &[(0, "k"), (0, "k"), (0, "none")]);
        assert_eq!(put(b.clone()), 3);
        assert_eq!(put(b), 3, "a re-replace purges first, so it skips as well");
        // A reference elsewhere re-resolved because an export key changed is not fresh.
        let skipped = put(frag("a.tx", &["A", "A2"], &[("k", 1), ("none", 0)], &[]));
        assert_eq!(skipped, 0);
    }

    #[test]
    fn prepared_apply_equals_replace() {
        let (t1, t2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (c1, s1) = cache(&t1);
        let (c2, s2) = cache(&t2);
        for f in sequence() {
            let mut tx = c1.write().unwrap();
            let d1 = s1.replace_fragment(&mut tx, f.clone()).unwrap();
            tx.commit().unwrap();
            let mut tx = c2.write().unwrap();
            let p = Stitcher::prepare(f).unwrap();
            let d2 = s2.apply(&mut tx, p).unwrap();
            tx.commit().unwrap();
            assert_eq!(d1, d2);
            assert_eq!(
                c1.read().unwrap().dump_graph().unwrap(),
                c2.read().unwrap().dump_graph().unwrap()
            );
        }
    }
}
