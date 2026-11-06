//! Fields of TypeScript types (FEAT-101): interface members, class properties and the members
//! of a type alias of an object type, as `field` symbols with the neutral `field_type`,
//! `required` and `many` facts, read from the oxc syntax tree.

use crate::items::{Binding, Builder};
use crate::modules::{NS_FILE, export_candidates, static_key};
use katalogr_model::{Confidence, Fact, Id, Link, LinkKind, Name, Segment, Span};
use oxc_ast::ast::{
    PropertyDefinition, TSInterfaceDeclaration, TSLiteral, TSSignature, TSType,
    TSTypeAliasDeclaration, TSTypeName, TSTypeReference,
};
use oxc_ast_visit::{Visit, walk};
use oxc_span::GetSpan;
use std::collections::BTreeMap;

/// What the written type of a member says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shape {
    pub field_type: String,
    pub required: bool,
    pub many: bool,
}

fn shape_of(field_type: &str, required: bool, many: bool) -> Shape {
    Shape {
        field_type: field_type.to_string(),
        required,
        many,
    }
}

/// The dotted name of a type reference, as written, without type arguments.
fn type_name(n: &TSTypeName<'_>) -> String {
    match n {
        TSTypeName::IdentifierReference(i) => i.name.as_str().to_string(),
        TSTypeName::QualifiedName(q) => format!("{}.{}", type_name(&q.left), q.right.name),
        _ => "this".to_string(),
    }
}

/// The shape of `t`. `optional` is the member's `?`.
pub(crate) fn shape(t: &TSType<'_>, optional: bool) -> Shape {
    let mut s = shape_inner(t);
    if optional {
        s.required = false;
    }
    s
}

fn shape_inner(t: &TSType<'_>) -> Shape {
    match t {
        TSType::TSParenthesizedType(p) => shape_inner(&p.type_annotation),
        TSType::TSTypeOperatorType(o) => shape_inner(&o.type_annotation),
        TSType::TSStringKeyword(_) => shape_of("string", true, false),
        TSType::TSNumberKeyword(_) => shape_of("number", true, false),
        TSType::TSBigIntKeyword(_) => shape_of("integer", true, false),
        TSType::TSBooleanKeyword(_) => shape_of("boolean", true, false),
        TSType::TSAnyKeyword(_) | TSType::TSUnknownKeyword(_) => shape_of("any", true, false),
        TSType::TSLiteralType(l) => match &l.literal {
            TSLiteral::StringLiteral(_) | TSLiteral::TemplateLiteral(_) => {
                shape_of("string", true, false)
            }
            _ => shape_of("any", true, false),
        },
        TSType::TSTypeLiteral(_) => shape_of("object", true, false),
        TSType::TSArrayType(a) => {
            let mut s = shape_inner(&a.element_type);
            s.required = true;
            s.many = true;
            s
        }
        TSType::TSUnionType(u) => {
            let nullish = |t: &TSType<'_>| {
                matches!(t, TSType::TSUndefinedKeyword(_) | TSType::TSNullKeyword(_))
            };
            let rest: Vec<&TSType<'_>> = u.types.iter().filter(|t| !nullish(t)).collect();
            let optional = rest.len() != u.types.len();
            let mut s = if rest.len() == 1 {
                shape_inner(rest[0])
            } else if !rest.is_empty()
                && rest.iter().all(|t| {
                    matches!(t, TSType::TSLiteralType(l) if matches!(l.literal, TSLiteral::StringLiteral(_)))
                })
            {
                shape_of("string", true, false)
            } else {
                shape_of("any", true, false)
            };
            if optional {
                s.required = false;
            }
            s
        }
        TSType::TSTypeReference(r) => {
            let name = type_name(&r.type_name);
            let arg = || {
                r.type_arguments
                    .as_ref()
                    .and_then(|a| a.params.first())
                    .map(shape_inner)
            };
            match name.as_str() {
                "Array" | "ReadonlyArray" | "Set" => {
                    let mut s = arg().unwrap_or_else(|| shape_of("any", true, false));
                    s.required = true;
                    s.many = true;
                    s
                }
                "Uint8Array" | "ArrayBuffer" => shape_of("binary", true, false),
                "Record" | "Map" => shape_of("object", true, false),
                "Date" => shape_of("datetime", true, false),
                _ => shape_of(&name, true, false),
            }
        }
        _ => shape_of("any", true, false),
    }
}

/// A named type a field mentions, resolved once every declaration of the file is known.
pub(crate) struct TypeRef {
    /// The identifier, or the namespace of a qualified name.
    pub name: String,
    /// The member of a qualified name.
    pub member: Option<String>,
    pub span: Span,
}

struct Refs<'b, 's> {
    b: &'b Builder<'s>,
    out: Vec<TypeRef>,
}

impl<'a> Visit<'a> for Refs<'_, '_> {
    fn visit_ts_type_reference(&mut self, it: &TSTypeReference<'a>) {
        let (name, member) = match &it.type_name {
            TSTypeName::IdentifierReference(i) => (i.name.as_str().to_string(), None),
            TSTypeName::QualifiedName(q) => match &q.left {
                TSTypeName::IdentifierReference(i) => (
                    i.name.as_str().to_string(),
                    Some(q.right.name.as_str().to_string()),
                ),
                _ => (String::new(), None),
            },
            _ => (String::new(), None),
        };
        if !name.is_empty() {
            self.out.push(TypeRef {
                name,
                member,
                span: self.b.span(it.span),
            });
        }
        walk::walk_ts_type_reference(self, it);
    }
}

impl Builder<'_> {
    /// One `field` symbol below the owner, linked from it with `contains`.
    #[allow(clippy::too_many_arguments)]
    fn add_field(
        &mut self,
        owner: (Id, &str, &str),
        key: &str,
        flags: (bool, bool, bool),
        ty: Option<&TSType<'_>>,
        span: oxc_span::Span,
        sig_end: u32,
    ) {
        let (owner_id, owner_kind, owner_name) = owner;
        let (optional, readonly, is_static) = flags;
        let sh = ty.map_or_else(|| shape_of("any", true, false), |t| shape(t, optional));
        let mut facts: BTreeMap<String, Fact> = BTreeMap::new();
        facts.insert("field_type".into(), Fact::Str(sh.field_type));
        facts.insert("required".into(), Fact::Bool(sh.required && !optional));
        facts.insert("many".into(), Fact::Bool(sh.many));
        if readonly {
            facts.insert("readonly".into(), Fact::Bool(true));
        }
        if is_static {
            facts.insert("static".into(), Fact::Bool(true));
        }
        let text = self
            .text
            .get(span.start as usize..sig_end as usize)
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        facts.insert(
            "signature".into(),
            Fact::Str(text.trim_end_matches([';', ',']).trim_end().to_string()),
        );
        let sp = self.span(span);
        let segments = vec![
            Segment {
                kind: owner_kind.to_string(),
                name: owner_name.to_string(),
            },
            Segment {
                kind: "field".to_string(),
                name: key.to_string(),
            },
        ];
        let id = self.add_symbol(segments, "field", sp, facts);
        self.contains(owner_id, id, sp);
        if let Some(t) = ty {
            let mut refs = Refs {
                b: self,
                out: Vec::new(),
            };
            refs.visit_ts_type(t);
            let out = refs.out;
            self.field_refs.extend(out.into_iter().map(|r| (id, r)));
        }
    }

    /// The property signatures of an interface body or an object type.
    fn signature_fields(&mut self, owner: (Id, &str, &str), members: &[TSSignature<'_>]) {
        for m in members {
            let TSSignature::TSPropertySignature(p) = m else {
                continue;
            };
            if p.computed {
                continue;
            }
            let Some(key) = static_key(&p.key) else {
                continue;
            };
            let ty = p.type_annotation.as_ref().map(|a| &a.type_annotation);
            let end = ty.map_or(p.key.span().end, |_| {
                p.type_annotation
                    .as_ref()
                    .map_or(p.span.end, |a| a.span.end)
            });
            self.add_field(
                owner,
                &key,
                (p.optional, p.readonly, false),
                ty,
                p.span,
                end,
            );
        }
    }

    pub(crate) fn interface_fields(
        &mut self,
        owner: Id,
        name: &str,
        i: &TSInterfaceDeclaration<'_>,
    ) {
        self.signature_fields((owner, "interface", name), &i.body.body);
    }

    /// A type alias of an object type, or of an intersection of object types.
    pub(crate) fn type_alias_fields(
        &mut self,
        owner: Id,
        name: &str,
        t: &TSTypeAliasDeclaration<'_>,
    ) {
        fn parts<'t, 'a>(t: &'t TSType<'a>, out: &mut Vec<&'t TSType<'a>>) {
            match t {
                TSType::TSParenthesizedType(p) => parts(&p.type_annotation, out),
                TSType::TSIntersectionType(i) => i.types.iter().for_each(|x| parts(x, out)),
                other => out.push(other),
            }
        }
        let mut ps = Vec::new();
        parts(&t.type_annotation, &mut ps);
        for p in ps {
            if let TSType::TSTypeLiteral(l) = p {
                self.signature_fields((owner, "type", name), &l.members);
            }
        }
    }

    /// A class property. A `#x` private name keeps its `#`.
    pub(crate) fn class_property_field(
        &mut self,
        owner: Id,
        name: &str,
        p: &PropertyDefinition<'_>,
    ) {
        if p.computed {
            return;
        }
        let Some(key) = static_key(&p.key) else {
            return;
        };
        let ty = p.type_annotation.as_ref().map(|a| &a.type_annotation);
        let end = p
            .type_annotation
            .as_ref()
            .map_or_else(|| p.key.span().end, |a| a.span.end);
        self.add_field(
            owner_tuple(owner, name),
            &key,
            (p.optional, p.readonly, p.r#static),
            ty,
            p.span,
            end,
        );
    }

    /// Resolve the type names of the fields: a type declared in this file by a link, an
    /// imported one through a relative or aliased module by a reference. A package import or
    /// a global emits nothing.
    pub(crate) fn resolve_field_types(&mut self) {
        let mut seen: std::collections::HashSet<(Id, String)> = std::collections::HashSet::new();
        for (from, r) in std::mem::take(&mut self.field_refs) {
            match (&r.member, self.imports.get(&r.name).cloned()) {
                (None, Some(Binding::Named { modules, name })) => {
                    if modules.iter().any(|m| m.namespace == NS_FILE) {
                        let cands = export_candidates(&modules, &name);
                        let k = format!("{:?}", cands);
                        if seen.insert((from, k)) {
                            self.push_ref(
                                from,
                                "uses-type",
                                Some(r.span),
                                cands,
                                Confidence::Extracted,
                            );
                        }
                    }
                }
                (Some(m), Some(Binding::Namespace { modules })) => {
                    if modules.iter().any(|k| k.namespace == NS_FILE) {
                        let cands = export_candidates(&modules, m);
                        let k = format!("{:?}", cands);
                        if seen.insert((from, k)) {
                            self.push_ref(
                                from,
                                "uses-type",
                                Some(r.span),
                                cands,
                                Confidence::Extracted,
                            );
                        }
                    }
                }
                (None, None) => {
                    let Some(&to) = self.declared.get(&r.name) else {
                        continue;
                    };
                    let is_type = self.symbols.iter().any(|s| {
                        s.name.id() == to
                            && matches!(s.kind.as_str(), "interface" | "type" | "class" | "enum")
                    });
                    if is_type && seen.insert((from, r.name.clone())) {
                        self.links.push(Link {
                            from,
                            to,
                            kind: LinkKind("uses-type".to_string()),
                            confidence: Confidence::Extracted,
                            span: Some(r.span),
                            provenance: self.provenance.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
    }
}

fn owner_tuple(owner: Id, name: &str) -> (Id, &str, &str) {
    (owner, "class", name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    fn shapes(src: &str) -> Vec<(String, bool, bool)> {
        let alloc = Allocator::default();
        let text = format!("interface X {{ {src} }}");
        let ret = Parser::new(&alloc, &text, SourceType::ts()).parse();
        let oxc_ast::ast::Statement::TSInterfaceDeclaration(i) = &ret.program.body[0] else {
            panic!()
        };
        i.body
            .body
            .iter()
            .filter_map(|m| match m {
                TSSignature::TSPropertySignature(p) => {
                    let s = shape(&p.type_annotation.as_ref()?.type_annotation, p.optional);
                    Some((s.field_type, s.required, s.many))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn mapping() {
        let got = shapes(
            "a: string; b?: number; c: string | null; d: Line[]; e: Array<Line>; \
             f: Uint8Array; g: Record<string, number>; h: Date; i: 'x' | 'y'; j: any; \
             k: A | B; l: ns.Line; m: bigint; n: boolean; o: { z: 1 };",
        );
        let t = |s: &str, r: bool, m: bool| (s.to_string(), r, m);
        assert_eq!(
            got,
            [
                t("string", true, false),
                t("number", false, false),
                t("string", false, false),
                t("Line", true, true),
                t("Line", true, true),
                t("binary", true, false),
                t("object", true, false),
                t("datetime", true, false),
                t("string", true, false),
                t("any", true, false),
                t("any", true, false),
                t("ns.Line", true, false),
                t("integer", true, false),
                t("boolean", true, false),
                t("object", true, false),
            ]
        );
    }
}
