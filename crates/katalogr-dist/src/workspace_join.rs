//! Attaching the joins of a project's peers to a reply (FEAT-020): the far ends of what `refs`,
//! `callers` and `dangling` found, each peer's state, and the exact and partial rules.

use crate::workspace::{OpenPeer, fingerprint, index_fix};
use crate::{DistError, Katalogr, Options, Outcome, Reply};
use katalogr_model::{AnyName, Id, LinkKind, Name, parse_name};
use katalogr_query::{
    DanglingRef, LinkKeys, Partial, PeerEnd, PeerState, ResolvedRef, Wanted, WorkspaceBlock, cap,
    parse_wanted, peer_contract_ends, peer_export_ends,
};
use katalogr_store::{ReadTx, Reader, StoreError};
use std::collections::BTreeSet;
use std::time::Instant;

impl Katalogr {
    /// The peers a read joins: none when the project names none or the read says `no_peers`.
    /// A project that reads peers adds their fingerprint to the question's view-ID arguments.
    pub(crate) fn peers_for_read(&self, opts: &Options) -> Result<Vec<OpenPeer>, DistError> {
        if opts.no_peers {
            return Ok(Vec::new());
        }
        let deadline = Instant::now() + opts.timeout()?;
        let peers = self.open_peers(deadline)?;
        if !peers.is_empty() {
            self.view_extra
                .lock()
                .expect("view_extra lock")
                .push(("workspace".to_string(), fingerprint(&peers)));
        }
        Ok(peers)
    }
}

/// Marks a peer whose snapshot could not be read: an I/O or store error never fails the local
/// answer.
fn mark_unavailable(p: &mut OpenPeer) {
    p.state.state = "unavailable".to_string();
    p.state.fix = Some(index_fix(&p.spec));
    p.engine = None;
}

/// One snapshot per open peer, in order, and the peers whose snapshot could not be read.
pub(crate) fn snapshots(peers: &[OpenPeer]) -> (Vec<Option<ReadTx<'_>>>, BTreeSet<usize>) {
    let mut failed = BTreeSet::new();
    let snaps = peers
        .iter()
        .enumerate()
        .map(|(i, p)| match p.engine.as_ref().map(|e| e.cache().read()) {
            Some(Ok(s)) => Some(s),
            Some(Err(_)) => {
                failed.insert(i);
                None
            }
            None => None,
        })
        .collect();
    (snaps, failed)
}

/// The ends of `keys` in every open peer.
pub(crate) fn peer_ends(peers: &mut [OpenPeer], keys: &LinkKeys) -> Vec<PeerEnd> {
    let (snaps, mut failed) = snapshots(peers);
    let mut out = Vec::new();
    for (i, (p, snap)) in peers.iter().zip(&snaps).enumerate() {
        let Some(tx) = snap else { continue };
        let read = || -> Result<Vec<PeerEnd>, StoreError> {
            let mut ends = Vec::new();
            for id in &keys.contracts {
                ends.extend(peer_contract_ends(tx, &p.spec.project, &p.access, *id)?);
            }
            if !keys.exports.is_empty() {
                ends.extend(peer_export_ends(
                    tx,
                    &p.spec.project,
                    &p.access,
                    &keys.exports,
                )?);
            }
            Ok(ends)
        };
        match read() {
            Ok(e) => out.extend(e),
            Err(_) => {
                failed.insert(i);
            }
        }
    }
    drop(snaps);
    for i in failed {
        mark_unavailable(&mut peers[i]);
    }
    out
}

/// The contracts a symbol provides, for `callers`.
pub(crate) fn provided(tx: &ReadTx, id: Id) -> Result<Vec<Id>, StoreError> {
    let mut out = BTreeSet::new();
    for key in tx.link_keys_out(id, &LinkKind("provides".to_string()))? {
        if tx.contract(key.to)?.is_some() {
            out.insert(key.to);
        }
    }
    Ok(out.into_iter().collect())
}

/// The contract ids named by `via` texts (`contract:<kind>:<key>`).
pub(crate) fn via_contracts<'a>(via: impl Iterator<Item = &'a str>) -> Vec<Id> {
    let mut out = BTreeSet::new();
    for v in via {
        if let Ok(AnyName::Contract(c)) = parse_name(v) {
            out.insert(c.id());
        }
    }
    out.into_iter().collect()
}

/// Entries of `list` that a peer resolves, moved out into `ResolvedRef`s.
pub(crate) fn resolve_dangling(
    peers: &mut [OpenPeer],
    list: Vec<DanglingRef>,
) -> (Vec<DanglingRef>, Vec<ResolvedRef>) {
    let (snaps, mut failed) = snapshots(peers);
    let mut keep = Vec::new();
    let mut resolved = Vec::new();
    for r in list {
        let mut ends: Vec<PeerEnd> = Vec::new();
        for text in &r.wanted {
            let Some(w) = parse_wanted(text) else {
                continue;
            };
            for (i, (p, snap)) in peers.iter().zip(&snaps).enumerate() {
                let Some(tx) = snap else { continue };
                let found = match &w {
                    Wanted::Contract(id) => peer_contract_ends(tx, &p.spec.project, &p.access, *id)
                        .map(|v| {
                            v.into_iter()
                                .filter(|e| match r.kind.as_str() {
                                    "consumes" => e.role == "provides",
                                    "provides" => e.role == "consumes",
                                    _ => true,
                                })
                                .collect::<Vec<_>>()
                        }),
                    Wanted::Export(k) => {
                        peer_export_ends(tx, &p.spec.project, &p.access, std::slice::from_ref(k))
                    }
                };
                match found {
                    Ok(v) => ends.extend(v),
                    Err(_) => {
                        failed.insert(i);
                    }
                }
            }
        }
        if ends.is_empty() {
            keep.push(r);
        } else {
            sort_ends(&mut ends);
            resolved.push(ResolvedRef { reference: r, ends });
        }
    }
    drop(snaps);
    for i in failed {
        mark_unavailable(&mut peers[i]);
    }
    (keep, resolved)
}

/// Sorted by link, project, role, name; opaque after visible.
pub(crate) fn sort_ends(ends: &mut Vec<PeerEnd>) {
    ends.sort_by(|a, b| {
        let key = |e: &PeerEnd| {
            (
                e.link.clone(),
                e.project.clone(),
                e.role.clone(),
                e.hit.as_ref().map(|h| h.name.clone()),
                e.opaque,
            )
        };
        key(a).cmp(&key(b))
    });
    ends.dedup();
}

/// One warning for a peer that was not read in full, or `None`. Names the project and the fix,
/// never a path.
fn warning(s: &PeerState) -> Option<String> {
    let who = match &s.kindex {
        Some(a) => format!("{a}/{}", s.project),
        None => s.project.clone(),
    };
    let mut parts = Vec::new();
    if s.state != "ok" {
        parts.push(format!("not read ({})", s.state));
    }
    if !s.repos_absent.is_empty() {
        parts.push(format!(
            "repos not indexed on this machine: {}",
            s.repos_absent.join(", ")
        ));
    }
    if s.stale {
        parts.push("has changes not yet indexed".to_string());
    }
    if parts.is_empty() {
        return None;
    }
    let mut text = format!("peer {who}: {}", parts.join("; "));
    if let Some(fix) = &s.fix {
        text.push_str(&format!("; fix: {fix}"));
    }
    Some(text)
}

/// Sets `Reply::workspace` and applies the exact and partial rules: any named peer that is not
/// `ok`, is stale or has absent repos clears `exact` and the view ID; a peer not read in full adds
/// a `peers` partial; each such peer adds one warning. Does nothing without peers.
pub(crate) fn attach<T>(
    reply: &mut Reply<T>,
    peers: &[OpenPeer],
    mut ends: Vec<PeerEnd>,
    resolved: Vec<ResolvedRef>,
) {
    if peers.is_empty() {
        return;
    }
    sort_ends(&mut ends);
    let states: Vec<PeerState> = peers.iter().map(|p| p.state.clone()).collect();
    if states
        .iter()
        .any(|s| s.state != "ok" || s.stale || !s.repos_absent.is_empty())
    {
        reply.report.exact = false;
        reply.report.view_id = None;
    }
    let named = states.len() as u64;
    let read = states
        .iter()
        .filter(|s| s.state == "ok" && s.repos_absent.is_empty())
        .count() as u64;
    if read < named {
        reply
            .partial
            .push(Partial::new(cap::PEERS, named, read, Some(named)));
    }
    reply
        .report
        .warnings
        .extend(states.iter().filter_map(warning));
    reply.workspace = Some(WorkspaceBlock {
        peers: states,
        ends,
        resolved,
    });
}

impl<T> Reply<T> {
    pub(crate) fn is_found(&self) -> bool {
        matches!(self.outcome, Outcome::Found(_))
    }
}
