//! `why`: the symbol at a line, and the rationale linked to it.

use crate::resolve::hit;
use crate::result::{RationaleLink, Resolution, SymbolHit, WhyResult};
use katalogr_model::{ArtifactName, Id, LinkKind, Name, RelPath};
use katalogr_store::{Reader, StoreError};

/// Link kinds that carry rationale (board items, commits, documents).
pub const RATIONALE_KINDS: &[&str] = &["refs-item", "annotated-by", "documented-by", "changed-by"];

/// The innermost symbol of `repo:path` whose span contains `line`, with its outgoing links of
/// kinds in [`RATIONALE_KINDS`]. Ties on equal span size are reported as ambiguous.
pub fn why(
    tx: &impl Reader,
    repo: &str,
    path: &RelPath,
    line: u32,
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
    for (key, _) in tx.links_out(id, None)? {
        if !RATIONALE_KINDS.contains(&key.kind.0.as_str()) {
            continue;
        }
        rationale.push(rationale_link(tx, key.kind, key.to)?);
    }
    rationale.sort_by(|a, b| (&a.kind, &a.target).cmp(&(&b.kind, &b.target)));
    Ok(Resolution::One(WhyResult {
        symbol: hit(&sym),
        rationale,
    }))
}

fn rationale_link(tx: &impl Reader, kind: LinkKind, to: Id) -> Result<RationaleLink, StoreError> {
    Ok(match tx.symbol(to)? {
        Some(t) => {
            let h: SymbolHit = hit(&t);
            RationaleLink {
                kind: kind.0,
                label: t
                    .name
                    .segments
                    .last()
                    .map_or_else(String::new, |s| s.name.clone()),
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
