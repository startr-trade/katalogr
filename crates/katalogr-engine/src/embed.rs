//! The embed pass (FEAT-017, ADR-0021): connects host extractors that offer literals to guest
//! extensions that bind them. Technology-neutral: the engine knows ids and versions only.

use crate::error::EngineError;
use crate::registry::ExtensionRegistry;
use katalogr_model::{
    Binding, Confidence, EmbeddedInput, EmbeddedText, ExtractError, Fragment, Name, RelPath,
};
use katalogr_store::{ReadTx, Reader};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Cache meta key of the guest signature.
pub(crate) const META_SIGNATURE: &str = "embed_signature";

/// Run every guest (registration order, excluding `host_id`) over `embedded`, in source order.
/// Bindings are normalised: symbol := owner, confidence := Inferred, span := Some(text span)
/// when None, origin := Some("embedded:<guest id>"). Bindings whose contract fails
/// `ContractName::validate` are dropped and reported as errors. Duplicates of
/// (contract canonical, role, symbol) are dropped. Results are appended to `fragment.bindings`.
pub(crate) fn apply(
    registry: &ExtensionRegistry,
    repo: &str,
    host_path: &RelPath,
    host_id: &str,
    embedded: &[EmbeddedText],
    contexts: &GuestContexts,
    fragment: &mut Fragment,
) -> Vec<ExtractError> {
    let mut errors = Vec::new();
    if embedded.is_empty() {
        return errors;
    }
    let ext = host_path
        .as_str()
        .rsplit('/')
        .next()
        .and_then(|n| n.rsplit_once('.'))
        .map_or("", |(_, e)| e);
    let guests: Vec<_> = registry
        .extractors()
        .filter(|g| g.id() != host_id && g.embedded_version().is_some())
        .collect();
    if guests.is_empty() {
        return errors;
    }
    let mut seen: BTreeSet<(String, katalogr_model::ContractRole, katalogr_model::Id)> = fragment
        .bindings
        .iter()
        .map(|b| (b.contract.canonical(), b.role, b.symbol))
        .collect();
    for text in embedded {
        for g in &guests {
            if !g.accepts_embedded(&text.text) {
                continue;
            }
            let input = EmbeddedInput {
                repo,
                host_path,
                host_extension: ext,
                owner: text.owner,
                span: text.span,
                text: &text.text,
                holes: &text.holes,
                call: text.call.as_ref(),
                context: contexts.get(g.id()),
            };
            for mut b in g.extract_embedded(&input) {
                if let Err(e) = b.contract.validate() {
                    errors.push(ExtractError::Parse {
                        span: Some(text.span),
                        message: format!("embedded binding refused: {e}"),
                    });
                    continue;
                }
                b.symbol = text.owner;
                b.confidence = Confidence::Inferred;
                b.span.get_or_insert(text.span);
                b.origin = Some(format!("embedded:{}", g.id()));
                if seen.insert((b.contract.canonical(), b.role, b.symbol)) {
                    fragment.bindings.push(Binding { ..b });
                }
            }
        }
    }
    errors
}

/// SHA-256 over "<id>\0<version>\n" for each guest, sorted by id; None when no guest exists.
pub(crate) fn signature(registry: &ExtensionRegistry) -> Option<[u8; 32]> {
    let mut guests: Vec<(&str, &str)> = registry
        .extractors()
        .filter_map(|e| e.embedded_version().map(|v| (e.id(), v)))
        .collect();
    if guests.is_empty() {
        return None;
    }
    guests.sort();
    let mut h = Sha256::new();
    for (id, v) in guests {
        h.update(id.as_bytes());
        h.update([0]);
        h.update(v.as_bytes());
        h.update(*b"\n");
    }
    Some(h.finalize().into())
}

/// The projected contexts of every guest (excluding `host_id`) for one repository.
#[derive(Default)]
pub(crate) struct GuestContexts {
    by_guest: BTreeMap<&'static str, Vec<u8>>,
}

impl GuestContexts {
    /// For each guest: `guest.embedded_context(stored context bytes of (repo, guest id), or &[])`.
    pub(crate) fn load(
        registry: &ExtensionRegistry,
        snap: &ReadTx,
        repo: &str,
        host_id: &str,
    ) -> Result<GuestContexts, EngineError> {
        let mut by_guest = BTreeMap::new();
        for g in registry.extractors() {
            if g.id() == host_id || g.embedded_version().is_none() {
                continue;
            }
            let stored = snap
                .ext_context(repo, g.id())?
                .map(|(_, b)| b)
                .unwrap_or_default();
            by_guest.insert(g.id(), g.embedded_context(&stored));
        }
        Ok(GuestContexts { by_guest })
    }

    /// The guest's projected bytes, or &[] when it has none.
    pub(crate) fn get(&self, guest_id: &str) -> &[u8] {
        self.by_guest.get(guest_id).map_or(&[], |v| v.as_slice())
    }

    /// Bytes mixed into host revisions: for each guest with a non-empty projection, sorted by id,
    /// "<id>\0" + u64 little-endian length + bytes. Empty when every projection is empty.
    pub(crate) fn key_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (id, b) in &self.by_guest {
            if b.is_empty() {
                continue;
            }
            out.extend_from_slice(id.as_bytes());
            out.push(0);
            out.extend_from_slice(&(b.len() as u64).to_le_bytes());
            out.extend_from_slice(b);
        }
        out
    }
}
