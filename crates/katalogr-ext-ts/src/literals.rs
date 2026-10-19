//! String and template literals offered to guest extensions (FEAT-017, ADR-0021), with the
//! call they are an argument of.

use crate::lines::Lines;
use katalogr_model::{CallContext, EmbeddedText, Id, SymbolNode, innermost_owner, offer};
use oxc_ast::ast::{
    Argument, CallExpression, Expression, NewExpression, Program, StringLiteral,
    TaggedTemplateExpression, TemplateLiteral,
};
use oxc_ast_visit::{Visit, walk};
use std::collections::BTreeMap;

/// All offers of `program`, in source order.
pub fn collect(
    program: &Program<'_>,
    lines: &Lines<'_>,
    symbols: &[SymbolNode],
    artifact: Id,
) -> Vec<EmbeddedText> {
    let mut w = Walk {
        calls: BTreeMap::new(),
        found: Vec::new(),
    };
    w.visit_program(program);
    w.found.sort_by_key(|(start, _, _, _)| *start);
    let mut out = Vec::new();
    for (start, end, text, holes) in w.found {
        let span = lines.span(start, end);
        let owner = innermost_owner(symbols, artifact, span);
        let call = w.calls.get(&start).cloned();
        offer(
            &mut out,
            EmbeddedText {
                owner,
                span,
                text,
                holes,
                call,
            },
        );
    }
    out
}

struct Walk {
    /// Call context by the start offset of the literal it applies to.
    calls: BTreeMap<u32, CallContext>,
    /// `(start, end, text, holes)`.
    found: Vec<(u32, u32, String, Vec<u32>)>,
}

/// A receiver is rendered only for an identifier, `this`, or a static member chain of those.
fn render_recv(e: &Expression<'_>) -> Option<String> {
    match e {
        Expression::Identifier(i) => Some(i.name.to_string()),
        Expression::ThisExpression(_) => Some("this".to_string()),
        Expression::StaticMemberExpression(m) => {
            Some(format!("{}.{}", render_recv(&m.object)?, m.property.name))
        }
        _ => None,
    }
}

/// The callee as written: an identifier, or `<recv>.<property>` with the receiver omitted when
/// it is not renderable. Anything else is not a callee we describe.
fn render_callee(e: &Expression<'_>) -> Option<String> {
    match e {
        Expression::Identifier(i) => Some(i.name.to_string()),
        Expression::StaticMemberExpression(m) => Some(match render_recv(&m.object) {
            Some(r) => format!("{r}.{}", m.property.name),
            None => format!(".{}", m.property.name),
        }),
        _ => None,
    }
}

impl Walk {
    /// Record the context of each argument that is, by itself, a string or template literal.
    fn args(&mut self, callee: &str, arguments: &[Argument<'_>]) {
        for (i, a) in arguments.iter().enumerate() {
            let Some(e) = a.as_expression() else {
                continue; // a spread counts as one position
            };
            let start = match e.get_inner_expression() {
                Expression::StringLiteral(s) => s.span.start,
                Expression::TemplateLiteral(t) => t.span.start,
                _ => continue,
            };
            if let Some(c) = CallContext::new(callee, i as u32) {
                self.calls.insert(start, c);
            }
        }
    }
}

impl<'a> Visit<'a> for Walk {
    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        if let Some(callee) = render_callee(&it.callee) {
            self.args(&callee, &it.arguments);
        }
        walk::walk_call_expression(self, it);
    }

    fn visit_new_expression(&mut self, it: &NewExpression<'a>) {
        if let Some(callee) = render_callee(&it.callee) {
            self.args(&format!("new {callee}"), &it.arguments);
        }
        walk::walk_new_expression(self, it);
    }

    fn visit_tagged_template_expression(&mut self, it: &TaggedTemplateExpression<'a>) {
        if let Some(tag) = render_callee(&it.tag)
            && let Some(c) = CallContext::new(&tag, 0)
        {
            self.calls.insert(it.quasi.span.start, c);
        }
        walk::walk_tagged_template_expression(self, it);
    }

    fn visit_string_literal(&mut self, it: &StringLiteral<'a>) {
        self.found
            .push((it.span.start, it.span.end, it.value.to_string(), Vec::new()));
    }

    fn visit_template_literal(&mut self, it: &TemplateLiteral<'a>) {
        let mut text = String::new();
        let mut holes = Vec::new();
        for (i, q) in it.quasis.iter().enumerate() {
            let piece = q.value.cooked.as_ref().unwrap_or(&q.value.raw);
            text.push_str(piece.as_str());
            if i < it.expressions.len() {
                holes.push(text.len() as u32);
            }
        }
        self.found.push((it.span.start, it.span.end, text, holes));
        walk::walk_template_literal(self, it);
    }
}
