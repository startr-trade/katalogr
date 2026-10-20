//! The `str_calls` fact (FEAT-014 sections 6.3 and 6.4): calls that pass a token-like string
//! argument, rendered as normalised text with their span, and the syntax-level resolution of the
//! arguments that name a function. Nothing here names a protocol or a framework; profiles read
//! the text (ADR-0020).

use crate::bindings::segments_of;
use crate::extract::Builder;
use crate::literals::{render_path, render_recv};
use katalogr_model::StrCall;
use katalogr_model::calls::{self, ArgRef};
use ra_ap_syntax::{
    AstNode, AstToken, SyntaxElement, SyntaxKind,
    ast::{self, HasArgList},
};

/// Deepest nesting of calls rendered inside an argument; deeper ones are `_`.
const MAX_DEPTH: u32 = 2;
/// Longest chain of calls rendered through receivers inside one argument.
const MAX_HOPS: u32 = 16;

/// An argument rendered, and whether it is a verbatim quoted value.
struct Arg {
    text: String,
    quoted: bool,
}

impl Arg {
    fn plain(text: impl Into<String>) -> Arg {
        Arg {
            text: text.into(),
            quoted: false,
        }
    }
}

type Refs = Vec<(String, ArgRef, bool)>;

/// A string value as it is rendered: redacted, then quoted when token-like, else `"…"`.
fn string_arg(value: &str) -> Arg {
    let v = calls::redact(value);
    if calls::token_like(&v) {
        Arg {
            text: calls::quote(&v),
            quoted: true,
        }
    } else {
        Arg::plain("\"…\"")
    }
}

/// A format string with `{{` and `}}` unescaped and every other `{…}` replaced by `{}`.
fn format_holes(fmt: &str) -> String {
    let mut out = String::with_capacity(fmt.len());
    let mut it = fmt.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' if it.peek() == Some(&'{') => {
                it.next();
                out.push('{');
            }
            '{' => {
                for d in it.by_ref() {
                    if d == '}' {
                        break;
                    }
                }
                out.push_str("{}");
            }
            '}' if it.peek() == Some(&'}') => {
                it.next();
                out.push('}');
            }
            c => out.push(c),
        }
    }
    out
}

/// The decoded value of a string literal expression.
fn string_value(e: &ast::Expr) -> Option<String> {
    let ast::Expr::Literal(l) = e else {
        return None;
    };
    match l.kind() {
        ast::LiteralKind::String(s) => s.value().ok().map(|v| v.into_owned()),
        _ => None,
    }
}

/// `"x".to_string()`, `.to_owned()`, `.into()` and `String::from("x")`: the inner string.
fn converted_string(e: &ast::Expr) -> Option<String> {
    match e {
        ast::Expr::MethodCallExpr(m) => {
            let name = m.name_ref()?.text().to_string();
            if !matches!(name.as_str(), "to_string" | "to_owned" | "into")
                || m.arg_list()?.args().next().is_some()
            {
                return None;
            }
            string_value(&m.receiver()?)
        }
        ast::Expr::CallExpr(c) => {
            let ast::Expr::PathExpr(p) = c.expr()? else {
                return None;
            };
            if segments_of(&p.path()?)? != ["String", "from"] {
                return None;
            }
            let mut args = c.arg_list()?.args();
            let first = args.next()?;
            args.next().is_none().then(|| string_value(&first))?
        }
        _ => None,
    }
}

/// The format string of a `format!` invocation, unescaped, holes as `{}`.
fn format_macro(m: &ast::MacroExpr) -> Option<String> {
    let mac = m.macro_call()?;
    if mac.path()?.segment()?.name_ref()?.text() != "format" {
        return None;
    }
    let tt = mac.token_tree()?;
    let tok = tt.syntax().children_with_tokens().find_map(|el| match el {
        SyntaxElement::Token(t)
            if !matches!(t.kind(), SyntaxKind::L_PAREN | SyntaxKind::WHITESPACE) =>
        {
            Some(t)
        }
        _ => None,
    })?;
    let s = ast::String::cast(tok)?;
    Some(format_holes(&s.value().ok()?))
}

impl Builder<'_> {
    /// The `str_calls` entry of a path call, when one of its arguments is a quoted value.
    pub(crate) fn str_call_of_call(&self, call: &ast::CallExpr) -> Option<StrCall> {
        let start: usize = call.expr()?.syntax().text_range().start().into();
        self.entry(&ast::Expr::CallExpr(call.clone()), start, call.syntax())
    }

    /// The `str_calls` entry of a method call, when one of its arguments is a quoted value.
    pub(crate) fn str_call_of_method(&self, call: &ast::MethodCallExpr) -> Option<StrCall> {
        let start: usize = call.name_ref()?.syntax().text_range().start().into();
        self.entry(
            &ast::Expr::MethodCallExpr(call.clone()),
            start,
            call.syntax(),
        )
    }

    fn entry(
        &self,
        e: &ast::Expr,
        start: usize,
        node: &ra_ap_syntax::SyntaxNode,
    ) -> Option<StrCall> {
        let mut refs = Refs::new();
        let (text, quoted) = self.render_call(e, 0, 0, &mut refs)?;
        if !quoted {
            return None;
        }
        let end: usize = node.text_range().end().into();
        refs.truncate(calls::ARG_REFS_MAX);
        Some(StrCall {
            text: calls::cap_text(text),
            span: self.span_of_range(start, end),
            refs,
        })
    }

    /// `callee(args)`, and whether an argument of this call is a quoted value.
    fn render_call(
        &self,
        e: &ast::Expr,
        depth: u32,
        hops: u32,
        refs: &mut Refs,
    ) -> Option<(String, bool)> {
        match e {
            ast::Expr::CallExpr(c) => {
                let ast::Expr::PathExpr(pe) = c.expr()? else {
                    return None;
                };
                let callee = render_path(&pe.path()?);
                let (args, quoted) = self.render_args(&c.arg_list()?, depth, refs);
                Some((format!("{callee}({args})"), quoted))
            }
            ast::Expr::MethodCallExpr(m) => {
                let name = m.name_ref()?.text().to_string();
                let prefix = match m.receiver() {
                    Some(r) => match render_recv(&r) {
                        Some(t) => t,
                        // Inside an argument, a receiver that is itself a call is rendered.
                        None if depth >= 1
                            && hops < MAX_HOPS
                            && matches!(
                                r,
                                ast::Expr::CallExpr(_) | ast::Expr::MethodCallExpr(_)
                            ) =>
                        {
                            self.render_call(&r, depth, hops + 1, refs)
                                .map(|x| x.0)
                                .unwrap_or_default()
                        }
                        None => String::new(),
                    },
                    None => String::new(),
                };
                let (args, quoted) = self.render_args(&m.arg_list()?, depth, refs);
                Some((format!("{prefix}.{name}({args})"), quoted))
            }
            _ => None,
        }
    }

    fn render_args(&self, list: &ast::ArgList, depth: u32, refs: &mut Refs) -> (String, bool) {
        let mut out: Vec<String> = Vec::new();
        let mut quoted = false;
        for a in list.args() {
            let r = self.render_arg(&a, depth, refs);
            quoted |= r.quoted;
            out.push(r.text);
        }
        (out.join(", "), quoted)
    }

    fn render_arg(&self, e: &ast::Expr, depth: u32, refs: &mut Refs) -> Arg {
        if let Some(v) = converted_string(e) {
            return string_arg(&v);
        }
        match e {
            ast::Expr::Literal(l) => match l.kind() {
                ast::LiteralKind::String(s) => match s.value() {
                    Ok(v) => string_arg(&v),
                    Err(_) => Arg::plain("_"),
                },
                ast::LiteralKind::IntNumber(_)
                | ast::LiteralKind::FloatNumber(_)
                | ast::LiteralKind::Bool(_) => Arg::plain(l.syntax().text().to_string()),
                _ => Arg::plain("_"),
            },
            ast::Expr::MacroExpr(m) => match format_macro(m) {
                Some(f) => string_arg(&f),
                None => Arg::plain("_"),
            },
            ast::Expr::RefExpr(r) => match r.expr() {
                Some(inner) => self.render_arg(&inner, depth, refs),
                None => Arg::plain("_"),
            },
            ast::Expr::ParenExpr(p) => match p.expr() {
                Some(inner) => self.render_arg(&inner, depth, refs),
                None => Arg::plain("_"),
            },
            ast::Expr::PathExpr(p) => {
                let Some(path) = p.path() else {
                    return Arg::plain("_");
                };
                let text = render_path(&path);
                if let Some(segs) = segments_of(&path) {
                    let cands = self.candidates(&segs, false);
                    if !cands.is_empty() && !refs.iter().any(|(t, _, _)| *t == text) {
                        refs.push((text.clone(), ArgRef::Candidates(cands), false));
                    }
                }
                Arg::plain(text)
            }
            ast::Expr::FieldExpr(_) => match render_recv(e) {
                Some(t) => Arg::plain(t),
                None => Arg::plain("_"),
            },
            ast::Expr::CallExpr(_) | ast::Expr::MethodCallExpr(_) => {
                if depth < MAX_DEPTH {
                    match self.render_call(e, depth + 1, 0, refs) {
                        Some((t, _)) => Arg::plain(t),
                        None => Arg::plain("_"),
                    }
                } else {
                    Arg::plain("_")
                }
            }
            _ => Arg::plain("_"),
        }
    }
}
