//! Joins over one peer's snapshot (FEAT-020). Pure: the caller opens the peer, says which of
//! its repos the user may read, and reads one snapshot. The core names no technology; contract
//! kinds and export namespaces are opaque strings.

use crate::resolve::hit;
use crate::result::SymbolHit;
use katalogr_model::insight::is_external_kind;
use katalogr_model::{AnyName, ExportKey, Id, LinkKind, Name, Span, parse_name};
use katalogr_store::{Reader, StoreError};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeSet;

const ROLES: &[&str] = &["provides", "consumes", "declares"];
const WHOLE_FILE: Span = Span {
    start_line: 1,
    start_col: 1,
    end_line: 1,
    end_col: 1,
};

/// Permissions come from the source systems, never a model katalogr invents. Called with a repo
/// and a repo-relative path, so a source offering path-level answers can be used later.
pub trait SourceAccess {
    fn can_read(&self, project: &str, repo: &str, path: &str) -> bool;
}

/// One far end of a link. `hit` absent means opaque: only project, link and role are known.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
pub struct PeerEnd {
    pub project: String,
    /// `contract:<kind>:<key>`, or `namespace:key` for a match by export key.
    pub link: String,
    /// `provides`, `consumes`, `declares` or `exports`.
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit: Option<SymbolHit>,
    #[serde(default)]
    pub opaque: bool,
}

/// An end as the snapshot holds it, with the repo it lies in.
struct Raw {
    role: String,
    repo: String,
    hit: SymbolHit,
}

/// The node `id` as a hit and its repo: a symbol, or a whole artifact.
fn node(tx: &impl Reader, id: Id) -> Result<Option<(String, SymbolHit)>, StoreError> {
    if let Some(row) = tx.symbol(id)? {
        return Ok(Some((row.name.artifact.repo.clone(), hit(&row))));
    }
    Ok(tx.artifact(id)?.map(|a| {
        (
            a.name.repo.clone(),
            SymbolHit {
                name: a.name.canonical(),
                kind: "artifact".to_string(),
                path: a.name.path.as_str().to_string(),
                span: WHOLE_FILE,
            },
        )
    }))
}

/// Visible ends sorted by role then name, then one opaque end for each role that had a restricted
/// end, so the number of restricted ends does not show.
fn finish(project: &str, link: &str, raws: Vec<Raw>, access: &dyn SourceAccess) -> Vec<PeerEnd> {
    let mut visible = Vec::new();
    let mut hidden = BTreeSet::new();
    for r in raws {
        if access.can_read(project, &r.repo, &r.hit.path) {
            visible.push(PeerEnd {
                project: project.to_string(),
                link: link.to_string(),
                role: r.role,
                hit: Some(r.hit),
                opaque: false,
            });
        } else {
            hidden.insert(r.role);
        }
    }
    visible.sort_by(|a, b| {
        (&a.role, a.hit.as_ref().map(|h| &h.name)).cmp(&(&b.role, b.hit.as_ref().map(|h| &h.name)))
    });
    visible.dedup();
    visible.extend(hidden.into_iter().map(|role| PeerEnd {
        project: project.to_string(),
        link: link.to_string(),
        role,
        hit: None,
        opaque: true,
    }));
    visible
}

/// Every end of `contract` in this peer's snapshot. Visible ends sorted by role, then name.
/// All restricted ends of one role become one opaque end, so their number does not show.
pub fn peer_contract_ends(
    tx: &impl Reader,
    project: &str,
    access: &dyn SourceAccess,
    contract: Id,
) -> Result<Vec<PeerEnd>, StoreError> {
    let Some(row) = tx.contract(contract)? else {
        return Ok(Vec::new());
    };
    let mut raws = Vec::new();
    for key in tx.links_in(contract)? {
        let Some(role) = ROLES.iter().find(|r| **r == key.kind.0) else {
            continue;
        };
        if let Some((repo, hit)) = node(tx, key.from)? {
            raws.push(Raw {
                role: (*role).to_string(),
                repo,
                hit,
            });
        }
    }
    Ok(finish(project, &row.name.canonical(), raws, access))
}

/// Nodes in this snapshot exporting each key, except nodes of an external kind. Same rules.
pub fn peer_export_ends(
    tx: &impl Reader,
    project: &str,
    access: &dyn SourceAccess,
    keys: &[ExportKey],
) -> Result<Vec<PeerEnd>, StoreError> {
    let mut out = Vec::new();
    for key in keys {
        let mut raws = Vec::new();
        for id in tx.exports(key)? {
            if let Some(row) = tx.symbol(id)?
                && is_external_kind(&row.kind)
            {
                continue;
            }
            if let Some((repo, hit)) = node(tx, id)? {
                raws.push(Raw {
                    role: "exports".to_string(),
                    repo,
                    hit,
                });
            }
        }
        out.extend(finish(
            project,
            &format!("{}:{}", key.namespace, key.key),
            raws,
            access,
        ));
    }
    Ok(out)
}

/// What a target is bound to here: its contracts (itself, when a contract), and its export keys
/// when it is of an external kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkKeys {
    pub contracts: Vec<Id>,
    pub exports: Vec<ExportKey>,
}

pub fn link_keys(tx: &impl Reader, target: &SymbolHit) -> Result<LinkKeys, StoreError> {
    let id = Id::of(&target.name);
    let mut out = LinkKeys::default();
    if target.kind == crate::resolve::CONTRACT_KIND {
        if tx.contract(id)?.is_some() {
            out.contracts.push(id);
        }
        return Ok(out);
    }
    let mut contracts = BTreeSet::new();
    for role in ROLES {
        for key in tx.link_keys_out(id, &LinkKind((*role).to_string()))? {
            if tx.contract(key.to)?.is_some() {
                contracts.insert(key.to);
            }
        }
    }
    out.contracts = contracts.into_iter().collect();
    if is_external_kind(&target.kind)
        && let Some(row) = tx.symbol(id)?
        && let Some(art) = tx.artifact(row.artifact)?
    {
        let keys: BTreeSet<ExportKey> = art
            .exports
            .iter()
            .filter(|e| e.target == id)
            .map(|e| e.key.clone())
            .collect();
        out.exports = keys.into_iter().collect();
    }
    Ok(out)
}

/// `namespace:key` or a contract name, as `DanglingRef::wanted` holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    Contract(Id),
    Export(ExportKey),
}

pub fn parse_wanted(text: &str) -> Option<Wanted> {
    if let Ok(AnyName::Contract(c)) = parse_name(text) {
        return Some(Wanted::Contract(c.id()));
    }
    let (namespace, key) = text.split_once(':')?;
    if namespace.is_empty() || key.is_empty() {
        return None;
    }
    Some(Wanted::Export(ExportKey {
        namespace: namespace.to_string(),
        key: key.to_string(),
    }))
}
