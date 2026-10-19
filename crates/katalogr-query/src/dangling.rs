//! `dangling`: references that name something no artifact exports (FEAT-019, R-2 and R-6).

use crate::result::{DanglingRef, SymbolHit};
use katalogr_model::{Id, Name, OutRef, Span, annotate};
use katalogr_store::{Reader, StoreError};
use std::collections::BTreeSet;

/// The link kinds `dangling` reports when none is given: the ones whose target is expected to
/// exist (Markdown links, annotations, trailers).
pub const DEFAULT_KINDS: &[&str] = &["links-to", "annotated-by", "refs-item"];

/// Unresolved references of the given kinds, sorted by referrer, kind, then wanted keys. The
/// referrer is the symbol the reference starts at, or the artifact when it starts at the file
/// itself (then the span is the reference's own, or the first line).
pub fn dangling(tx: &impl Reader, kinds: &[&str]) -> Result<Vec<DanglingRef>, StoreError> {
    let mut out = Vec::new();
    let prefixes: BTreeSet<String> = tx
        .exports_in(annotate::PREFIX_NS)?
        .into_iter()
        .map(|(k, _)| k.key)
        .collect();
    let have_adrs = !tx.exports_in(annotate::ADR_NS)?.is_empty();
    for (_, r) in tx.unresolved_refs(None)? {
        if !kinds.contains(&r.kind.0.as_str()) {
            continue;
        }
        if matches!(r.kind.0.as_str(), "annotated-by" | "refs-item")
            && !names_board_id(&r, &prefixes, have_adrs)
        {
            continue;
        }
        let Some(from) = referrer(tx, r.from, r.span)? else {
            continue;
        };
        out.push(DanglingRef {
            from,
            kind: r.kind.0.clone(),
            wanted: r
                .candidates
                .iter()
                .map(|c| format!("{}:{}", c.namespace, c.key))
                .collect(),
        });
    }
    out.sort_by(|a, b| (&a.from.name, &a.kind, &a.wanted).cmp(&(&b.from.name, &b.kind, &b.wanted)));
    out.dedup();
    Ok(out)
}

/// An unresolved annotation or trailer reference is dangling only when its code looks like
/// one the board uses: its prefix is exported as `board-prefix`, or it is an ADR id and the
/// board has ADRs. Other mentions (`SP-001`, `UTF-8`, `R-1`) are not board ids and are dropped.
fn names_board_id(r: &OutRef, prefixes: &BTreeSet<String>, have_adrs: bool) -> bool {
    r.candidates.iter().any(|c| match c.namespace.as_str() {
        annotate::ADR_NS => have_adrs,
        annotate::ITEM_NS | annotate::REQ_NS => {
            let code = c.key.split('/').next().unwrap_or(&c.key);
            prefixes.contains(annotate::prefix_of(code))
        }
        _ => true,
    })
}

fn referrer(tx: &impl Reader, id: Id, span: Option<Span>) -> Result<Option<SymbolHit>, StoreError> {
    if let Some(s) = tx.symbol(id)? {
        return Ok(Some(crate::resolve::hit(&s)));
    }
    Ok(tx.artifact(id)?.map(|a| SymbolHit {
        name: a.name.canonical(),
        kind: "artifact".to_string(),
        path: a.name.path.as_str().to_string(),
        span: span.unwrap_or(Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
        }),
    }))
}
