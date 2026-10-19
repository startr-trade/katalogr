//! Rationale markers in comments (FEAT-030 section 6.5): each `NOTE:`, `WHY:` or `RATIONALE:`
//! marker becomes an entry of the `rationale` fact of the innermost symbol that holds the
//! comment, or of the file when no symbol does.

use crate::lines::LineIndex;
use katalogr_model::Id;
use katalogr_model::annotate::{RationaleMarker, rationale_markers};
use ra_ap_syntax::{NodeOrToken, SyntaxKind, SyntaxNode, TextRange};
use std::collections::HashMap;

/// Markers per owner (`None` is the file), in source order, each with the 1-based line of the
/// first line of its comment.
pub type Found = Vec<(Option<Id>, Vec<(u32, RationaleMarker)>)>;

struct Group {
    owner: Option<Id>,
    line_comment: bool,
    start: usize,
    end: usize,
    text: String,
}

fn is_line_comment(text: &str) -> bool {
    text.starts_with("//")
}

/// Walk every comment of the tree. `symbols` maps the range of each symbol's node to its id.
/// Adjacent line comments of one owner form a single comment, so a marker continues over them.
pub fn collect(
    root: &SyntaxNode,
    source: &str,
    lines: &LineIndex<'_>,
    symbols: &HashMap<TextRange, Id>,
) -> Found {
    let mut groups: Vec<Group> = Vec::new();
    for el in root.descendants_with_tokens() {
        let NodeOrToken::Token(tok) = el else {
            continue;
        };
        if !matches!(
            tok.kind(),
            SyntaxKind::COMMENT | SyntaxKind::INNER_DOC_COMMENT | SyntaxKind::OUTER_DOC_COMMENT
        ) {
            continue;
        }
        let owner = tok
            .parent_ancestors()
            .find_map(|n| symbols.get(&n.text_range()).copied());
        let range = tok.text_range();
        let (start, end): (usize, usize) = (range.start().into(), range.end().into());
        let text = tok.text().to_string();
        let line = is_line_comment(&text);
        if let Some(last) = groups.last_mut()
            && last.line_comment
            && line
            && last.owner == owner
            && source[last.end..start].chars().all(char::is_whitespace)
            && source[last.end..start].matches('\n').count() == 1
        {
            last.end = end;
            last.text.push('\n');
            last.text.push_str(&text);
            continue;
        }
        groups.push(Group {
            owner,
            line_comment: line,
            start,
            end,
            text,
        });
    }
    let mut out: Found = Vec::new();
    for g in groups {
        let markers = rationale_markers(&g.text);
        if markers.is_empty() {
            continue;
        }
        let line = lines.position(g.start).0;
        let slot = match out.iter().position(|(o, _)| *o == g.owner) {
            Some(i) => i,
            None => {
                out.push((g.owner, Vec::new()));
                out.len() - 1
            }
        };
        out[slot].1.extend(markers.into_iter().map(|m| (line, m)));
    }
    out
}
