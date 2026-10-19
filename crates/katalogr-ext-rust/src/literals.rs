//! String literals offered to guest extensions (FEAT-017, ADR-0021): every `STRING` token of
//! the file, including the token trees of macro calls, with the call it is an argument of.

use crate::lines::LineIndex;
use katalogr_model::{CallContext, EmbeddedText, Id, SymbolNode, innermost_owner, offer};
use ra_ap_syntax::{
    AstNode, AstToken, SourceFile, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken,
    ast::{self},
};

/// All offers of `tree`, in source order.
pub fn collect(
    tree: &SourceFile,
    lines: &LineIndex,
    symbols: &[SymbolNode],
    artifact: Id,
) -> Vec<EmbeddedText> {
    let mut out = Vec::new();
    for el in tree.syntax().descendants_with_tokens() {
        let SyntaxElement::Token(tok) = el else {
            continue;
        };
        if tok.kind() != SyntaxKind::STRING {
            continue;
        }
        let Some(s) = ast::String::cast(tok.clone()) else {
            continue;
        };
        let Ok(value) = s.value() else {
            continue;
        };
        let range = tok.text_range();
        let span = lines.span(range.start().into(), range.end().into());
        let owner = innermost_owner(symbols, artifact, span);
        let call = call_of(&tok);
        offer(
            &mut out,
            EmbeddedText {
                owner,
                span,
                text: value.into_owned(),
                holes: Vec::new(),
                call,
            },
        );
    }
    out
}

/// `render_path`: segments joined by `::`, generic arguments and whitespace removed.
fn render_path(path: &ast::Path) -> String {
    let mut segs = Vec::new();
    let mut p = Some(path.clone());
    while let Some(cur) = p {
        if let Some(seg) = cur.segment() {
            let mut text = String::new();
            for child in seg.syntax().children_with_tokens() {
                match &child {
                    SyntaxElement::Node(n)
                        if matches!(
                            n.kind(),
                            SyntaxKind::GENERIC_ARG_LIST | SyntaxKind::PARENTHESIZED_ARG_LIST
                        ) => {}
                    other => text.push_str(&other.to_string()),
                }
            }
            segs.push(text.split_whitespace().collect::<String>());
        }
        p = cur.qualifier();
    }
    segs.reverse();
    segs.join("::")
}

/// A receiver is rendered only for a path, `self`, or a field chain of those.
fn render_recv(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::PathExpr(p) => Some(render_path(&p.path()?)),
        ast::Expr::FieldExpr(f) => {
            let base = render_recv(&f.expr()?)?;
            Some(format!("{base}.{}", f.name_ref()?.text()))
        }
        _ => None,
    }
}

/// The call context of a `STRING` token, by the contract of `CallContext`.
fn call_of(tok: &SyntaxToken) -> Option<CallContext> {
    let parent = tok.parent()?;
    match parent.kind() {
        // A literal in a macro's token tree: only directly in the call's own tree.
        SyntaxKind::TOKEN_TREE => {
            let mac = ast::MacroCall::cast(parent.parent()?)?;
            let callee = format!("{}!", render_path(&mac.path()?));
            let start = tok.text_range().start();
            let arg = parent
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .filter(|t| t.kind() == SyntaxKind::COMMA && t.text_range().end() <= start)
                .count() as u32;
            CallContext::new(&callee, arg)
        }
        SyntaxKind::LITERAL => {
            // Climb parentheses and borrows.
            let mut cur: SyntaxNode = parent;
            let mut up = cur.parent()?;
            while matches!(up.kind(), SyntaxKind::PAREN_EXPR | SyntaxKind::REF_EXPR) {
                cur = up;
                up = cur.parent()?;
            }
            let args = ast::ArgList::cast(up)?;
            let arg = args.args().position(|a| a.syntax() == &cur)? as u32;
            let call = args.syntax().parent()?;
            let callee = match call.kind() {
                SyntaxKind::CALL_EXPR => {
                    let c = ast::CallExpr::cast(call)?;
                    match c.expr()? {
                        ast::Expr::PathExpr(p) => render_path(&p.path()?),
                        _ => return None,
                    }
                }
                SyntaxKind::METHOD_CALL_EXPR => {
                    let m = ast::MethodCallExpr::cast(call)?;
                    let name = m.name_ref()?.text().to_string();
                    match m.receiver().and_then(|r| render_recv(&r)) {
                        Some(r) => format!("{r}.{name}"),
                        None => format!(".{name}"),
                    }
                }
                _ => return None,
            };
            CallContext::new(&callee, arg)
        }
        _ => None,
    }
}
