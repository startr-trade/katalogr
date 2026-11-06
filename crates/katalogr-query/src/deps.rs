//! `deps`: the package versions a dependency inventory lists, with the members that pin each
//! and the code that uses the package (FEAT-007 R-8).

use crate::dangling::referrer;
use crate::result::SymbolHit;
use katalogr_model::insight::{KIND_PACKAGE_MEMBER, KIND_PACKAGE_VERSION};
use katalogr_model::{AnyName, Fact, Id, Name, parse_name};
use katalogr_store::{Reader, StoreError};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// One package version in one lockfile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DepHit {
    /// Canonical name of the version node.
    pub name: String,
    pub ecosystem: String,
    pub package: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub dev: bool,
    /// `<repo>:<path>` of the lockfile.
    pub lockfile: String,
    /// Canonical names of the members that pin this version, sorted.
    pub pinned_by: Vec<String>,
    /// What uses the package name, sorted, at most `used_by_limit`.
    pub used_by: Vec<SymbolHit>,
    /// What uses the package name, before the limit.
    pub used_by_total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepsOptions {
    /// Only this package name.
    pub package: Option<String>,
    pub limit: usize,
    pub used_by_limit: usize,
}

/// One candidate version node.
struct Cand {
    id: Id,
    name: String,
    package: String,
    version: String,
    lockfile: String,
    /// Canonical name of the package node above it.
    package_node: String,
    artifact: Id,
}

/// Package versions sorted by package, version, lockfile. The second value is all matches
/// before the limit.
pub fn deps(tx: &impl Reader, opts: &DepsOptions) -> Result<(Vec<DepHit>, u64), StoreError> {
    let mut cands: Vec<Cand> = Vec::new();
    for (id, core) in tx.symbol_cores()? {
        if core.kind != KIND_PACKAGE_VERSION {
            continue;
        }
        let Ok(AnyName::Symbol(sym)) = parse_name(&core.name) else {
            continue;
        };
        let [package, version] = sym.segments.as_slice() else {
            continue;
        };
        if opts.package.as_ref().is_some_and(|p| *p != package.name) {
            continue;
        }
        let lockfile = sym.artifact.canonical();
        cands.push(Cand {
            id,
            package_node: format!("{lockfile}#package:{}", escape_name(&package.name)),
            package: package.name.clone(),
            version: version.name.clone(),
            lockfile,
            name: core.name,
            artifact: core.artifact,
        });
    }
    cands.sort_by(|a, b| {
        (&a.package, &a.version, &a.lockfile, &a.name).cmp(&(
            &b.package,
            &b.version,
            &b.lockfile,
            &b.name,
        ))
    });
    let total = cands.len() as u64;
    cands.truncate(opts.limit);

    let ids: Vec<Id> = cands.iter().map(|c| c.id).collect();
    let rows = tx.symbols_many(&ids)?;
    let mut ecosystems: BTreeMap<Id, String> = BTreeMap::new();
    let mut users: BTreeMap<String, (Vec<SymbolHit>, u64)> = BTreeMap::new();
    let mut out = Vec::with_capacity(cands.len());
    for (c, row) in cands.into_iter().zip(rows) {
        let Some(row) = row else { continue };
        let ecosystem = match ecosystems.get(&c.artifact) {
            Some(e) => e.clone(),
            None => {
                let e = match tx
                    .artifact(c.artifact)?
                    .and_then(|a| a.facts.get("ecosystem").cloned())
                {
                    Some(Fact::Str(s)) => s,
                    _ => String::new(),
                };
                ecosystems.insert(c.artifact, e.clone());
                e
            }
        };
        let str_fact = |k: &str| match row.facts.get(k) {
            Some(Fact::Str(s)) => Some(s.clone()),
            _ => None,
        };

        // Members that pin this version.
        let mut pinned_by = BTreeSet::new();
        for key in tx.links_in(c.id)? {
            if key.kind.0 != "depends-on" {
                continue;
            }
            if let Some(from) = tx.symbol(key.from)?
                && from.kind == KIND_PACKAGE_MEMBER
            {
                pinned_by.insert(from.name.canonical());
            }
        }

        // Code that uses the package name; the same for each version of it.
        let (used_by, used_by_total) = match users.get(&c.package_node) {
            Some(u) => u.clone(),
            None => {
                let mut hits: BTreeMap<String, SymbolHit> = BTreeMap::new();
                for key in tx.links_in(Id::of(&c.package_node))? {
                    if key.kind.0 == katalogr_model::insight::LINK_HAS_VERSION {
                        continue;
                    }
                    let span = tx
                        .link(&key)?
                        .and_then(|r| r.entries.first().and_then(|e| e.span));
                    if let Some(h) = referrer(tx, key.from, span)? {
                        hits.entry(h.name.clone()).or_insert(h);
                    }
                }
                let total = hits.len() as u64;
                let mut list: Vec<SymbolHit> = hits.into_values().collect();
                list.truncate(opts.used_by_limit);
                users.insert(c.package_node.clone(), (list.clone(), total));
                (list, total)
            }
        };

        out.push(DepHit {
            name: c.name,
            ecosystem,
            package: c.package,
            version: str_fact("version").unwrap_or(c.version),
            source: str_fact("source"),
            dev: matches!(row.facts.get("dev"), Some(Fact::Bool(true))),
            lockfile: c.lockfile,
            pinned_by: pinned_by.into_iter().collect(),
            used_by,
            used_by_total,
        });
    }
    Ok((out, total))
}

/// The model's escaping of a name segment.
fn escape_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '/' | ':' | '#' | '~' | '%') || c.is_whitespace() {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}
