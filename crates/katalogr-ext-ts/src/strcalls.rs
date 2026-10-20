//! The `str_calls` fact (FEAT-014 sections 6.3 and 6.4): calls that pass a token-like string
//! argument, rendered as normalised text with their span, and the syntax-level resolution of the
//! arguments that name a function or method. Nothing here names a protocol or a framework;
//! profiles read the text (ADR-0020).

use crate::items::{Binding, Builder};
use crate::literals::render_recv;
use crate::modules::{NS_MEMBER, export_candidates, key};
use katalogr_model::calls::{self, ArgRef};
use katalogr_model::{Name, StrCall};
use oxc_ast::ast::{
    ArrayExpressionElement, BinaryOperator, CallExpression, Expression, ObjectPropertyKind,
    PropertyKey, TemplateLiteral,
};
use oxc_span::GetSpan;

/// Deepest nesting of calls, objects and arrays rendered inside an argument.
const MAX_DEPTH: u32 = 2;
/// Longest chain of calls rendered through receivers inside one argument.
const MAX_HOPS: u32 = 16;

type Refs = Vec<(String, ArgRef, bool)>;

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

/// The text of a template literal, every `${…}` as `{}`.
fn template_text(t: &TemplateLiteral<'_>) -> String {
    let mut out = String::new();
    for (i, q) in t.quasis.iter().enumerate() {
        out.push_str(q.value.cooked.as_ref().unwrap_or(&q.value.raw).as_str());
        if i < t.expressions.len() {
            out.push_str("{}");
        }
    }
    out
}

/// The text of a `+` concatenation: string and template operands verbatim, others `{}`. Sets
/// `has` when a string or template operand occurs.
fn concat_text(e: &Expression<'_>, has: &mut bool) -> String {
    match e.get_inner_expression() {
        Expression::StringLiteral(s) => {
            *has = true;
            s.value.to_string()
        }
        Expression::TemplateLiteral(t) => {
            *has = true;
            template_text(t)
        }
        Expression::BinaryExpression(b) if b.operator == BinaryOperator::Addition => {
            let l = concat_text(&b.left, has);
            let r = concat_text(&b.right, has);
            l + &r
        }
        _ => "{}".to_string(),
    }
}

/// Whether an argument can render as a quoted value: a string, a template, a `+` concatenation,
/// or an object with such a value of its own.
fn may_be_quoted(e: &Expression<'_>) -> bool {
    match e.get_inner_expression() {
        Expression::StringLiteral(_) | Expression::TemplateLiteral(_) => true,
        Expression::BinaryExpression(b) => b.operator == BinaryOperator::Addition,
        Expression::ObjectExpression(o) => o.properties.iter().any(|p| match p {
            ObjectPropertyKind::ObjectProperty(p) => matches!(
                p.value.get_inner_expression(),
                Expression::StringLiteral(_)
                    | Expression::TemplateLiteral(_)
                    | Expression::BinaryExpression(_)
            ),
            _ => false,
        }),
        _ => false,
    }
}

impl Builder<'_> {
    /// The `str_calls` entry of a call, when one of its arguments is a quoted value.
    pub(crate) fn str_call_of(&self, it: &CallExpression<'_>) -> Option<StrCall> {
        // Most calls pass no string: skip the rendering unless an argument can be one.
        if !it
            .arguments
            .iter()
            .any(|a| a.as_expression().is_some_and(may_be_quoted))
        {
            return None;
        }
        let mut refs = Refs::new();
        let (text, quoted) = self.render_call(it, 0, 0, &mut refs)?;
        if !quoted {
            return None;
        }
        // From the method name (member callee) or the callee to the closing parenthesis.
        let start = match it.callee.get_inner_expression() {
            Expression::StaticMemberExpression(m) => m.property.span.start,
            other => other.span().start,
        };
        refs.truncate(calls::ARG_REFS_MAX);
        Some(StrCall {
            text: calls::cap_text(text),
            span: self.span(oxc_span::Span::new(start, it.span.end)),
            refs,
        })
    }

    /// `callee(args)`, and whether an argument of this call is a quoted value. `None` for a
    /// callee that is neither an identifier nor a static member.
    fn render_call(
        &self,
        it: &CallExpression<'_>,
        depth: u32,
        hops: u32,
        refs: &mut Refs,
    ) -> Option<(String, bool)> {
        let callee = match it.callee.get_inner_expression() {
            Expression::Identifier(i) => i.name.to_string(),
            Expression::StaticMemberExpression(m) => {
                let obj = m.object.get_inner_expression();
                let recv = match render_recv(obj) {
                    Some(r) => r,
                    // Inside an argument, a receiver that is itself a call is rendered.
                    None => match obj {
                        Expression::CallExpression(c) if depth >= 1 && hops < MAX_HOPS => self
                            .render_call(c, depth, hops + 1, refs)
                            .map(|x| x.0)
                            .unwrap_or_default(),
                        _ => String::new(),
                    },
                };
                format!("{recv}.{}", m.property.name)
            }
            _ => return None,
        };
        let mut quoted = false;
        let mut args: Vec<String> = Vec::with_capacity(it.arguments.len());
        for a in &it.arguments {
            let r = match a.as_expression() {
                Some(e) => self.render_arg(e, depth, refs),
                None => Arg::plain("_"),
            };
            quoted |= r.quoted;
            args.push(r.text);
        }
        Some((format!("{callee}({})", args.join(", ")), quoted))
    }

    fn render_arg(&self, e: &Expression<'_>, depth: u32, refs: &mut Refs) -> Arg {
        let e = e.get_inner_expression();
        match e {
            Expression::StringLiteral(s) => string_arg(s.value.as_str()),
            Expression::TemplateLiteral(t) => string_arg(&template_text(t)),
            Expression::BinaryExpression(b) if b.operator == BinaryOperator::Addition => {
                let mut has = false;
                let text = concat_text(e, &mut has);
                if has {
                    string_arg(&text)
                } else {
                    Arg::plain("_")
                }
            }
            Expression::NumericLiteral(n) => Arg::plain(
                self.text
                    .get(n.span.start as usize..n.span.end as usize)
                    .unwrap_or("_"),
            ),
            Expression::BooleanLiteral(b) => Arg::plain(if b.value { "true" } else { "false" }),
            Expression::Identifier(i) => {
                let name = i.name.as_str();
                if let Some(r) = self.arg_ref_ident(name) {
                    push_ref(refs, name, r, false);
                }
                Arg::plain(name)
            }
            Expression::ThisExpression(_) => Arg::plain("this"),
            Expression::StaticMemberExpression(m) => match render_recv(e) {
                Some(text) => {
                    let (r, inferred) = self.arg_ref_member(&m.object, m.property.name.as_str());
                    push_ref(refs, &text, r, inferred);
                    Arg::plain(text)
                }
                None => Arg::plain("_"),
            },
            Expression::CallExpression(c) => {
                if depth < MAX_DEPTH {
                    match self.render_call(c, depth + 1, 0, refs) {
                        Some((t, _)) => Arg::plain(t),
                        None => Arg::plain("_"),
                    }
                } else {
                    Arg::plain("_")
                }
            }
            Expression::ObjectExpression(o) if depth <= MAX_DEPTH => {
                let mut props: Vec<(String, String)> = Vec::new();
                // A top-level object whose own values include a quoted one (a request config)
                // makes its call recorded too.
                let mut quoted = false;
                for p in &o.properties {
                    let ObjectPropertyKind::ObjectProperty(p) = p else {
                        continue;
                    };
                    if p.computed {
                        continue;
                    }
                    let k = match &p.key {
                        PropertyKey::StaticIdentifier(i) => i.name.to_string(),
                        PropertyKey::StringLiteral(s) => s.value.to_string(),
                        _ => continue,
                    };
                    let v = self.render_arg(&p.value, depth + 1, refs);
                    quoted |= depth == 0 && v.quoted;
                    props.push((k, v.text));
                }
                props.sort();
                let body: Vec<String> = props
                    .into_iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect();
                Arg {
                    text: format!("{{{}}}", body.join(", ")),
                    quoted,
                }
            }
            Expression::ArrayExpression(a) if depth <= MAX_DEPTH => {
                let items: Vec<String> = a
                    .elements
                    .iter()
                    .filter_map(|el| match el {
                        ArrayExpressionElement::SpreadElement(_)
                        | ArrayExpressionElement::Elision(_) => None,
                        other => other
                            .as_expression()
                            .map(|x| self.render_arg(x, depth + 1, refs).text),
                    })
                    .collect();
                Arg::plain(format!("[{}]", items.join(", ")))
            }
            _ => Arg::plain("_"),
        }
    }

    /// An identifier that names a function of this file, or a named import.
    fn arg_ref_ident(&self, name: &str) -> Option<ArgRef> {
        if let Some(&to) = self.callable.get(name) {
            let rels = self.rel_names.get_or_init(|| {
                self.symbols
                    .iter()
                    .filter_map(|s| {
                        let canon = s.name.canonical();
                        let (_, rel) = canon.split_once('#')?;
                        Some((s.name.id(), rel.to_string()))
                    })
                    .collect()
            });
            return rels.get(&to).map(|rel| ArgRef::Local(rel.clone()));
        }
        if let Some(Binding::Named { modules, name }) = self.imports.get(name) {
            let cands = export_candidates(modules, name);
            if !cands.is_empty() {
                return Some(ArgRef::Candidates(cands));
            }
        }
        None
    }

    /// `ns.f` through a namespace import resolves to the export; any other `o.m` to the member
    /// name, inferred.
    fn arg_ref_member(&self, object: &Expression<'_>, prop: &str) -> (ArgRef, bool) {
        if let Expression::Identifier(o) = object.get_inner_expression()
            && let Some(Binding::Namespace { modules }) = self.imports.get(o.name.as_str())
        {
            let cands = export_candidates(modules, prop);
            if !cands.is_empty() {
                return (ArgRef::Candidates(cands), false);
            }
        }
        (
            ArgRef::Candidates(vec![key(NS_MEMBER, prop.to_string())]),
            true,
        )
    }
}

fn push_ref(refs: &mut Refs, text: &str, r: ArgRef, inferred: bool) {
    if !refs.iter().any(|(t, _, _)| t == text) {
        refs.push((text.to_string(), r, inferred));
    }
}
