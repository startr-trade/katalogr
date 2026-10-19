//! Rationale markers in comments (FEAT-030 section 6.5): each `NOTE:`, `WHY:` or `RATIONALE:`
//! marker becomes an entry of the `rationale` fact of the innermost symbol that holds the
//! comment, else of the symbol that follows it, else of the file.

use crate::lines::Lines;
use katalogr_model::annotate::{RationaleMarker, rationale_markers};
use katalogr_model::{Id, Name, Span, SymbolNode};
use oxc_ast::ast::{Comment, CommentKind};

/// Markers per owner (`None` is the file), each with the 1-based line of the first line of its
/// comment, in source order.
pub type Found = Vec<(Option<Id>, Vec<(u32, RationaleMarker)>)>;

struct Block {
    start: usize,
    end: usize,
    line_comment: bool,
}

/// `a` holds `b`.
fn holds(a: &Span, b: &Span) -> bool {
    (a.start_line, a.start_col) <= (b.start_line, b.start_col)
        && (b.end_line, b.end_col) <= (a.end_line, a.end_col)
}

/// Adjacent line comments (only whitespace with one line break between them) form one comment,
/// so a marker continues over them.
fn blocks(comments: &[Comment], text: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    for c in comments {
        let (start, end) = (c.span.start as usize, c.span.end as usize);
        let line_comment = c.kind == CommentKind::Line;
        if let Some(last) = out.last_mut()
            && last.line_comment
            && line_comment
            && text[last.end..start].chars().all(char::is_whitespace)
            && text[last.end..start].matches('\n').count() == 1
        {
            last.end = end;
            continue;
        }
        out.push(Block {
            start,
            end,
            line_comment,
        });
    }
    out
}

fn is_comment_line(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') || t.ends_with("*/")
}

/// Attribute every marker of `comments`. `symbols` are the symbols of the file.
pub fn collect(
    comments: &[Comment],
    text: &str,
    lines: &Lines<'_>,
    symbols: &[SymbolNode],
) -> Found {
    let source_lines: Vec<&str> = text.lines().collect();
    let mut out: Found = Vec::new();
    for b in blocks(comments, text) {
        let raw = &text[b.start..b.end];
        let markers = rationale_markers(raw);
        if markers.is_empty() {
            continue;
        }
        let span = lines.span(b.start as u32, b.end as u32);
        // 1. The innermost symbol whose span holds the comment.
        let holder = symbols
            .iter()
            .filter(|s| holds(&s.span, &span))
            .max_by_key(|s| (s.span.start_line, s.span.start_col));
        // 2. The symbol that starts on the first code line after the comment.
        let owner = holder.or_else(|| {
            let next = (span.end_line as usize..source_lines.len())
                .find(|&i| {
                    let l = source_lines[i];
                    !l.trim().is_empty() && !is_comment_line(l)
                })
                .map(|i| i as u32 + 1)?;
            symbols
                .iter()
                .filter(|s| s.span.start_line == next)
                .min_by_key(|s| {
                    (
                        s.span.start_col,
                        std::cmp::Reverse((s.span.end_line, s.span.end_col)),
                    )
                })
        });
        let id = owner.map(|s| s.name.id());
        let slot = match out.iter().position(|(o, _)| *o == id) {
            Some(i) => i,
            None => {
                out.push((id, Vec::new()));
                out.len() - 1
            }
        };
        out[slot]
            .1
            .extend(markers.into_iter().map(|m| (span.start_line, m)));
    }
    out
}
