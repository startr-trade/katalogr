//! `doc_impact`: which documents are linked to code that changed (FEAT-086).
//!
//! The caller learns the changed files and line ranges from git; the query reads only the graph.
//! A *document* is any artifact on the other end of a listed link kind, so the query names no
//! format.

use crate::dangling::referrer;
use crate::result::{PARTIAL_NOTE, Partial, SymbolHit, cap};
use katalogr_model::{ArtifactName, Id, LinkKind, Name, RelPath};
use katalogr_store::{Reader, StoreError};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Link kinds that connect a document to code, as the document extractors emit them today.
pub const DOC_LINK_KINDS: &[&str] = &["links-to", "documented-by", "annotated-by", "specifies"];

/// The kinds whose outgoing links from the code name the document at the other end.
const OUTGOING_KINDS: &[&str] = &["documented-by", "annotated-by"];

/// The query crate's own plain copy of the engine's `FileChange`; dist converts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: RelPath,
    /// `added`, `modified` or `deleted`.
    pub kind: String,
    /// Changed line ranges in the new version (1-based, inclusive).
    pub ranges: Vec<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub struct DocImpactInput<'a> {
    /// The repository the changes belong to.
    pub repo: &'a str,
    pub changes: &'a [ChangedFile],
    /// Link kinds that connect a document to code; empty means [`DOC_LINK_KINDS`].
    pub kinds: &'a [String],
    /// Most documents to report; `0` means no cap.
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DocImpact {
    pub docs: Vec<AffectedDoc>,
    pub changed_files: u64,
    pub partial: Vec<Partial>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AffectedDoc {
    /// The document artifact.
    pub doc: SymbolHit,
    /// The document itself changed in the same range, so it may already be up to date.
    pub doc_changed: bool,
    pub links: Vec<AffectedLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
pub struct AffectedLink {
    /// Canonical name of the section or document the link starts from.
    pub from: String,
    pub kind: String,
    /// Canonical name of the target, or the wanted key when the target is gone.
    pub target: String,
    /// `modified`, `added` or `deleted`.
    pub change: String,
    /// `symbol` when a changed line falls inside the target's span; `file` when the link names
    /// the whole file.
    pub precision: String,
    /// Repository-relative path of the changed file the target is in.
    pub path: String,
}

/// One thing in a changed file that a document may link to.
struct Target {
    id: Id,
    name: String,
    precision: &'static str,
    change: String,
    path: String,
    /// The changed file's artifact, which the document must differ from.
    file: Id,
}

/// A document end found for a target: the end's own id (symbol or artifact), its kind, and the
/// link kind.
struct Source {
    end: Id,
    kind: String,
}

/// Every document linked to changed code, sorted by document then target. Pure and
/// deterministic.
pub fn doc_impact(tx: &impl Reader, input: &DocImpactInput<'_>) -> Result<DocImpact, StoreError> {
    let kinds: BTreeSet<String> = if input.kinds.is_empty() {
        DOC_LINK_KINDS.iter().map(|k| k.to_string()).collect()
    } else {
        input.kinds.iter().cloned().collect()
    };
    let changed: BTreeSet<&str> = input.changes.iter().map(|c| c.path.as_str()).collect();

    let mut targets: Vec<Target> = Vec::new();
    let mut deleted: Vec<&ChangedFile> = Vec::new();
    for c in input.changes {
        match c.kind.as_str() {
            "deleted" => deleted.push(c),
            _ => targets_in_change(tx, input.repo, c, &mut targets)?,
        }
    }

    // links per document artifact
    let mut by_doc: BTreeMap<Id, Vec<AffectedLink>> = BTreeMap::new();
    let ids: Vec<Id> = targets.iter().map(|t| t.id).collect();
    for (t, sources) in targets.iter().zip(doc_sources(tx, &ids, &kinds)?) {
        for s in sources {
            let (doc_art, from_name) = match end_of(tx, s.end)? {
                Some(e) => e,
                None => continue,
            };
            if doc_art == t.file {
                continue;
            }
            by_doc.entry(doc_art).or_default().push(AffectedLink {
                from: from_name,
                kind: s.kind,
                target: t.name.clone(),
                change: t.change.clone(),
                precision: t.precision.to_string(),
                path: t.path.clone(),
            });
        }
    }

    if !deleted.is_empty() {
        for (_, r) in tx.unresolved_refs(None)? {
            if !kinds.contains(&r.kind.0) {
                continue;
            }
            let Some((doc_art, from_name)) = end_of(tx, r.from)? else {
                continue;
            };
            for c in &deleted {
                let p = c.path.as_str();
                for cand in &r.candidates {
                    if names_path(&cand.key, p) {
                        by_doc.entry(doc_art).or_default().push(AffectedLink {
                            from: from_name.clone(),
                            kind: r.kind.0.clone(),
                            target: format!("{}:{}", cand.namespace, cand.key),
                            change: "deleted".to_string(),
                            precision: "file".to_string(),
                            path: p.to_string(),
                        });
                    }
                }
            }
        }
    }

    let mut docs = Vec::new();
    for (doc_art, mut links) in by_doc {
        let Some(hit) = referrer(tx, doc_art, None)? else {
            continue;
        };
        links.sort_by(|a, b| {
            (&a.target, &a.kind, &a.from, &a.precision).cmp(&(
                &b.target,
                &b.kind,
                &b.from,
                &b.precision,
            ))
        });
        links.dedup();
        let doc_changed = tx
            .artifact(doc_art)?
            .is_some_and(|a| a.name.repo == input.repo && changed.contains(a.name.path.as_str()));
        docs.push(AffectedDoc {
            doc: hit,
            doc_changed,
            links,
        });
    }
    docs.sort_by(|a, b| a.doc.name.cmp(&b.doc.name));

    let mut partial = Vec::new();
    if input.limit > 0 && docs.len() > input.limit {
        let total = docs.len() as u64;
        docs.truncate(input.limit);
        partial.push(Partial {
            cap: cap::LIMIT.to_string(),
            at: input.limit as u64,
            shown: docs.len() as u64,
            total: Some(total),
            note: PARTIAL_NOTE.to_string(),
        });
    }
    Ok(DocImpact {
        docs,
        changed_files: input.changes.len() as u64,
        partial,
    })
}

/// Whether the wanted `key` names `path` itself or a place inside it (`path#anchor`).
pub(crate) fn names_path(key: &str, path: &str) -> bool {
    key == path || key.strip_prefix(path).is_some_and(|r| r.starts_with('#'))
}

/// The document artifact an end belongs to, with the end's canonical name.
fn end_of(tx: &impl Reader, id: Id) -> Result<Option<(Id, String)>, StoreError> {
    if let Some(s) = tx.symbol(id)? {
        return Ok(Some((s.artifact, s.name.canonical())));
    }
    Ok(tx.artifact(id)?.map(|a| (id, a.name.canonical())))
}

/// For one added or modified file: its artifact (`file` precision), plus, when modified, the
/// symbols whose span contains a changed line (`symbol` precision).
fn targets_in_change(
    tx: &impl Reader,
    repo: &str,
    c: &ChangedFile,
    out: &mut Vec<Target>,
) -> Result<(), StoreError> {
    let name = ArtifactName {
        repo: repo.to_string(),
        path: c.path.clone(),
    };
    let file = name.id();
    let Some(row) = tx.artifact(file)? else {
        return Ok(());
    };
    out.push(Target {
        id: file,
        name: row.name.canonical(),
        precision: "file",
        change: c.kind.clone(),
        path: c.path.as_str().to_string(),
        file,
    });
    if c.kind != "modified" || c.ranges.is_empty() {
        return Ok(());
    }
    for (id, sym) in row.symbols.iter().zip(tx.symbols_many(&row.symbols)?) {
        let Some(sym) = sym else { continue };
        let (s, e) = (sym.span.start_line, sym.span.end_line);
        if c.ranges.iter().any(|(a, b)| *a <= e && s <= *b) {
            out.push(Target {
                id: *id,
                name: sym.name.canonical(),
                precision: "symbol",
                change: c.kind.clone(),
                path: c.path.as_str().to_string(),
                file,
            });
        }
    }
    Ok(())
}

/// For each target, the document ends linked to it: `links_in` of the listed kinds (which
/// includes fan targets), and `links_out` of `documented-by` and `annotated-by`.
fn doc_sources(
    tx: &impl Reader,
    ids: &[Id],
    kinds: &BTreeSet<String>,
) -> Result<Vec<Vec<Source>>, StoreError> {
    let mut out: Vec<Vec<Source>> = Vec::with_capacity(ids.len());
    for keys in tx.links_in_many(ids)? {
        out.push(
            keys.into_iter()
                .filter(|k| kinds.contains(&k.kind.0))
                .map(|k| Source {
                    end: k.from,
                    kind: k.kind.0,
                })
                .collect(),
        );
    }
    for kind in OUTGOING_KINDS.iter().filter(|k| kinds.contains(**k)) {
        let lk = LinkKind(kind.to_string());
        for (slot, keys) in out.iter_mut().zip(tx.link_keys_out_many(ids, &lk)?) {
            slot.extend(keys.into_iter().map(|k| Source {
                end: k.to,
                kind: k.kind.0,
            }));
        }
    }
    Ok(out)
}
