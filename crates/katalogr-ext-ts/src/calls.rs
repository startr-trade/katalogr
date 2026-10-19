//! Call sites (FEAT-006 section 4): `f()` through a local or imported binding is extracted,
//! `ns.f()` through a namespace import is extracted, `obj.m()` is inferred from the
//! `("ts-member", "m")` candidate, and `require("x")` / `import("x")` are imports.

use crate::items::{Binding, Builder, Pending};
use crate::modules::{NS_MEMBER, export_candidates, key};
use katalogr_model::{Confidence, Id, Link, LinkKind};
use oxc_ast::ast::{CallExpression, Expression, ImportExpression, NewExpression};
use oxc_ast_visit::{Visit, walk};

struct CallWalk<'b, 's> {
    b: &'b mut Builder<'s>,
    from: Id,
}

impl<'a> Visit<'a> for CallWalk<'_, '_> {
    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        let span = self.b.span(it.span);
        if let Some(spec) = it
            .callee
            .get_inner_expression()
            .get_identifier_reference()
            .filter(|i| i.name.as_str() == "require")
            .and(require_spec_of(it))
        {
            let modules = self.b.modules_for(spec);
            self.b.import_ref(modules, span);
        } else {
            self.b.call(self.from, &it.callee, span);
        }
        walk::walk_call_expression(self, it);
    }

    /// `new X()`: a call to the class `X`, resolved like a call to `X`.
    fn visit_new_expression(&mut self, it: &NewExpression<'a>) {
        let span = self.b.span(it.span);
        self.b.call(self.from, &it.callee, span);
        walk::walk_new_expression(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        if let Expression::StringLiteral(s) = it.source.get_inner_expression() {
            let span = self.b.span(it.span);
            let modules = self.b.modules_for(s.value.as_str());
            self.b.import_ref(modules, span);
        }
        walk::walk_import_expression(self, it);
    }
}

/// The specifier of a `require("x")` call expression.
fn require_spec_of<'x>(c: &'x CallExpression<'_>) -> Option<&'x str> {
    if c.arguments.len() != 1 {
        return None;
    }
    match c.arguments[0].as_expression()?.get_inner_expression() {
        Expression::StringLiteral(s) => Some(s.value.as_str()),
        _ => None,
    }
}

impl Builder<'_> {
    fn call(&mut self, from: Id, callee: &Expression<'_>, span: katalogr_model::Span) {
        match callee.get_inner_expression() {
            Expression::Identifier(id) => {
                let name = id.name.as_str();
                if let Some(&to) = self.callable.get(name) {
                    self.links.push(Link {
                        from,
                        to,
                        kind: LinkKind("calls".to_string()),
                        confidence: Confidence::Extracted,
                        span: Some(span),
                        provenance: self.provenance.clone(),
                    });
                } else if let Some(Binding::Named { modules, name }) = self.imports.get(name) {
                    let cands = export_candidates(modules, name);
                    if !cands.is_empty() {
                        self.push_ref(from, "calls", Some(span), cands, Confidence::Extracted);
                    }
                }
            }
            Expression::StaticMemberExpression(m) => {
                let prop = m.property.name.as_str();
                let ns = match m.object.get_inner_expression() {
                    Expression::Identifier(o) => match self.imports.get(o.name.as_str()) {
                        Some(Binding::Namespace { modules }) => Some(modules.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                match ns {
                    Some(modules) => {
                        let cands = export_candidates(&modules, prop);
                        if !cands.is_empty() {
                            self.push_ref(from, "calls", Some(span), cands, Confidence::Extracted);
                        }
                    }
                    None => self.push_ref(
                        from,
                        "calls",
                        Some(span),
                        vec![key(NS_MEMBER, prop.to_string())],
                        Confidence::Inferred,
                    ),
                }
            }
            _ => {}
        }
    }

    /// Walk the code collected by the declaration pass.
    pub fn walk_calls(&mut self, pending: Vec<Pending<'_, '_>>) {
        let artifact = self.artifact_id;
        for p in pending {
            match p {
                Pending::Func(id, f) => {
                    let mut w = CallWalk { b: self, from: id };
                    w.visit_formal_parameters(&f.params);
                    if let Some(body) = &f.body {
                        w.visit_function_body(body);
                    }
                }
                Pending::Arrow(id, a) => {
                    CallWalk { b: self, from: id }.visit_arrow_function_expression(a);
                }
                Pending::ClassElement(id, el) => {
                    CallWalk { b: self, from: id }.visit_class_element(el);
                }
                Pending::Statement(s) => {
                    CallWalk {
                        b: self,
                        from: artifact,
                    }
                    .visit_statement(s);
                }
                Pending::Declarator(d) => {
                    CallWalk {
                        b: self,
                        from: artifact,
                    }
                    .visit_variable_declarator(d);
                }
                Pending::Expression(e) => {
                    CallWalk {
                        b: self,
                        from: artifact,
                    }
                    .visit_expression(e);
                }
            }
        }
    }
}
