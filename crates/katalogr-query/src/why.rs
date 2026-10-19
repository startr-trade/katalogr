//! `why`: the symbol at a line, and the rationale linked to it.

use crate::resolve::hit;
use crate::result::{CommitHit, RationaleLink, Resolution, SymbolHit, WhyResult};
use katalogr_model::{ArtifactName, Fact, Id, LinkKind, Name, RelPath, SymbolName};
use katalogr_store::{Reader, StoreError};
use std::collections::BTreeSet;

/// Link kinds that carry rationale (board items, commits, documents).
pub const RATIONALE_KINDS: &[&str] = &["refs-item", "annotated-by", "documented-by", "changed-by"];

const SERVES: &str = "serves";
const REFS_ITEM: &str = "refs-item";
/// Path prefix of commit artifacts (see `katalogr_engine::history`).
const HISTORY_PREFIX: &str = "@history/";

/// What the caller learned from git about the line (the query reads only the graph).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blame {
    /// Blame was not asked for.
    NotRequested,
    /// The commit (hex id) that introduced the line.
    Commit(String),
    /// Blame could not run; the reason is reported in the result.
    Unavailable(String),
}

/// The innermost symbol of `repo:path` whose span contains `line`, with its outgoing links of
/// kinds in [`RATIONALE_KINDS`], the commit that introduced the line (from `blame`) and the
/// goals served by every item reached. Ties on equal span size are reported as ambiguous.
pub fn why(
    tx: &impl Reader,
    repo: &str,
    path: &RelPath,
    line: u32,
    blame: &Blame,
) -> Result<Resolution<WhyResult>, StoreError> {
    let artifact = ArtifactName {
        repo: repo.to_string(),
        path: path.clone(),
    };
    let Some(row) = tx.artifact(artifact.id())? else {
        return Ok(Resolution::None);
    };
    let mut inside = Vec::new();
    for id in &row.symbols {
        if let Some(s) = tx.symbol(*id)?
            && s.span.start_line <= line
            && line <= s.span.end_line
        {
            inside.push((*id, s));
        }
    }
    let size = |s: &katalogr_store::SymbolRow| {
        (
            s.span.end_line - s.span.start_line,
            s.span.end_col as i64 - s.span.start_col as i64,
        )
    };
    let Some(best) = inside.iter().map(|(_, s)| size(s)).min() else {
        return Ok(Resolution::None);
    };
    let mut tied: Vec<_> = inside
        .into_iter()
        .filter(|(_, s)| size(s) == best)
        .collect();
    tied.sort_by_key(|(_, s)| s.name.canonical());
    if tied.len() > 1 {
        return Ok(Resolution::Ambiguous(
            tied.iter().map(|(_, s)| hit(s)).collect(),
        ));
    }
    let (id, sym) = tied.remove(0);
    let mut rationale = Vec::new();
    let mut reached: Vec<Id> = Vec::new();
    for (key, _) in tx.links_out(id, None)? {
        if !RATIONALE_KINDS.contains(&key.kind.0.as_str()) {
            continue;
        }
        reached.push(key.to);
        rationale.push(rationale_link(tx, key.kind, key.to)?);
    }

    let mut commits = Vec::new();
    let mut note = None;
    match blame {
        Blame::NotRequested => {}
        Blame::Unavailable(why) => note = Some(format!("commit section omitted: {why}")),
        Blame::Commit(sha) => {
            let name = ArtifactName {
                repo: repo.to_string(),
                path: RelPath::new(&format!("{HISTORY_PREFIX}{sha}"))
                    .map_err(|e| StoreError::Row(e.to_string()))?,
            };
            let cid = name.id();
            if let Some(c) = tx.artifact(cid)? {
                let mut items = Vec::new();
                let kind = LinkKind(REFS_ITEM.to_string());
                for (key, _) in tx.links_out(cid, Some(&kind))? {
                    reached.push(key.to);
                    let link = rationale_link(tx, LinkKind("changed-by".to_string()), key.to)?;
                    items.push(link.target.clone());
                    rationale.push(link);
                }
                items.sort();
                items.dedup();
                let summary = match c.facts.get("summary") {
                    Some(Fact::Str(s)) => s.clone(),
                    _ => String::new(),
                };
                commits.push(CommitHit {
                    sha: sha.clone(),
                    summary,
                    items,
                });
            }
        }
    }
    rationale.sort_by(|a, b| (&a.kind, &a.target).cmp(&(&b.kind, &b.target)));
    rationale.dedup();

    let goals = goals_of(tx, &reached)?;
    Ok(Resolution::One(WhyResult {
        symbol: hit(&sym),
        rationale,
        goals,
        commits,
        note,
        notes: Vec::new(),
    }))
}

/// The goals the items among `reached` serve. A requirement stands for its item.
fn goals_of(tx: &impl Reader, reached: &[Id]) -> Result<Vec<RationaleLink>, StoreError> {
    let serves = LinkKind(SERVES.to_string());
    let mut items = BTreeSet::new();
    for id in reached {
        let Some(t) = tx.symbol(*id)? else { continue };
        if t.name.segments.first().is_some_and(|s| s.kind == "item") {
            let item = SymbolName {
                artifact: t.name.artifact.clone(),
                segments: t.name.segments[..1].to_vec(),
                ordinal: 1,
            };
            items.insert(item.id());
        }
    }
    let mut out = Vec::new();
    for item in items {
        for (key, _) in tx.links_out(item, Some(&serves))? {
            out.push(rationale_link(tx, serves.clone(), key.to)?);
        }
    }
    out.sort_by(|a, b| (&a.kind, &a.target).cmp(&(&b.kind, &b.target)));
    out.dedup();
    Ok(out)
}

fn rationale_link(tx: &impl Reader, kind: LinkKind, to: Id) -> Result<RationaleLink, StoreError> {
    Ok(match tx.symbol(to)? {
        Some(t) => {
            let h: SymbolHit = hit(&t);
            let label = match t.name.segments.first() {
                // `FEAT-019/R-2`: a requirement is named with its item.
                Some(first) if first.kind == "item" => t
                    .name
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("/"),
                _ => t
                    .name
                    .segments
                    .last()
                    .map_or_else(String::new, |s| s.name.clone()),
            };
            RationaleLink {
                kind: kind.0,
                label,
                target: h.name,
            }
        }
        None => RationaleLink {
            kind: kind.0,
            label: to.to_hex(),
            target: to.to_hex(),
        },
    })
}
