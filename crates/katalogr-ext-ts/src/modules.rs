//! Module specifiers to ordered candidate export keys, and the import/require bindings of one
//! file (FEAT-006 section 4). Nothing here touches the filesystem: candidates come from the
//! [`TsContext`] and the first candidate that some file exports wins at stitch time.

use crate::context::{TsContext, join_norm, parent_dir};
use crate::items::{Binding, Builder};
use katalogr_model::{Confidence, ExportKey, LinkKind, Span};
use oxc_ast::ast::{
    BindingPattern, Expression, ImportDeclaration, ImportDeclarationSpecifier, ModuleExportName,
    PropertyKey, VariableDeclaration,
};

pub const NS_FILE: &str = "ts-file";
pub const NS_EXPORT: &str = "ts-export";
pub const NS_PACKAGE: &str = "ts-package";
pub const NS_MEMBER: &str = "ts-member";

pub fn key(namespace: &str, key: String) -> ExportKey {
    ExportKey {
        namespace: namespace.to_string(),
        key,
    }
}

/// The key under which a file exports a binding: `<path>#<name>`.
pub fn export_name(path: &str, name: &str) -> String {
    format!("{path}#{name}")
}

fn child(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// The files a path without a known extension may name, in order.
pub fn expand_file(p: &str) -> Vec<ExportKey> {
    let mut out = vec![p.to_string()];
    if !p.is_empty() {
        for ext in ["ts", "tsx", "d.ts", "js", "jsx", "mjs", "cjs"] {
            out.push(format!("{p}.{ext}"));
        }
    }
    for index in ["index.ts", "index.tsx", "index.js"] {
        out.push(child(p, index));
    }
    out.retain(|s| !s.is_empty());
    out.into_iter().map(|s| key(NS_FILE, s)).collect()
}

fn package_name_of(spec: &str) -> (&str, &str) {
    let mut it = spec.splitn(if spec.starts_with('@') { 3 } else { 2 }, '/');
    let first = it.next().unwrap_or("");
    if spec.starts_with('@') {
        let second = it.next();
        let name_len = first.len() + second.map_or(0, |s| s.len() + 1);
        (&spec[..name_len], spec.get(name_len + 1..).unwrap_or(""))
    } else {
        (first, spec.get(first.len() + 1..).unwrap_or(""))
    }
}

/// Match a `paths` pattern; the captured text for its single `*`.
fn match_pattern<'p>(pattern: &str, spec: &'p str) -> Option<&'p str> {
    match pattern.split_once('*') {
        None => (pattern == spec).then_some(""),
        Some((pre, post)) => spec
            .strip_prefix(pre)
            .and_then(|r| r.strip_suffix(post))
            .filter(|_| spec.len() >= pre.len() + post.len()),
    }
}

/// Ordered candidates (`ts-file` and `ts-package` keys) for `spec` as written in `from`.
pub fn module_candidates(ctx: &TsContext, from: &str, spec: &str) -> Vec<ExportKey> {
    let mut out: Vec<ExportKey> = Vec::new();
    if spec == "." || spec == ".." || spec.starts_with("./") || spec.starts_with("../") {
        if let Some(p) = join_norm(parent_dir(from), spec) {
            out.extend(expand_file(&p));
        }
    } else if !spec.is_empty() {
        if let Some(project) = ctx.alias_project(from) {
            // The longest matching pattern prefix wins, as in TypeScript.
            let best = project
                .paths
                .iter()
                .filter_map(|(pat, targets)| {
                    match_pattern(pat, spec)
                        .map(|cap| (pat.find('*').unwrap_or(pat.len()), cap, targets))
                })
                .max_by_key(|(prefix, _, _)| *prefix);
            if let Some((_, cap, targets)) = best {
                for t in targets {
                    out.extend(expand_file(&t.replacen('*', cap, 1)));
                }
            }
            if let Some(base) = &project.base_url
                && let Some(p) = join_norm(base, spec)
            {
                out.extend(expand_file(&p));
            }
        }
        let (name, sub) = package_name_of(spec);
        if let Some(ws) = ctx.workspace_package(name) {
            if sub.is_empty() {
                if let Some(entry) = &ws.entry {
                    out.extend(expand_file(entry));
                }
                for dir in [child(&ws.dir, "src"), ws.dir.clone()] {
                    for ext in ["ts", "tsx", "d.ts", "js", "jsx", "mjs", "cjs"] {
                        out.push(key(NS_FILE, child(&dir, &format!("index.{ext}"))));
                    }
                }
            } else {
                for dir in [child(&ws.dir, "src"), ws.dir.clone()] {
                    out.extend(expand_file(&child(&dir, sub)));
                }
            }
        }
        out.push(key(NS_PACKAGE, name.to_string()));
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|k| seen.insert(k.clone()));
    out
}

/// `("ts-export", "<file>#<name>")` for each `ts-file` candidate, in order.
pub fn export_candidates(modules: &[ExportKey], name: &str) -> Vec<ExportKey> {
    modules
        .iter()
        .filter(|k| k.namespace == NS_FILE)
        .map(|k| key(NS_EXPORT, export_name(&k.key, name)))
        .collect()
}

pub fn module_export_name<'a>(n: &'a ModuleExportName<'_>) -> &'a str {
    match n {
        ModuleExportName::IdentifierName(i) => i.name.as_str(),
        ModuleExportName::IdentifierReference(i) => i.name.as_str(),
        ModuleExportName::StringLiteral(s) => s.value.as_str(),
    }
}

/// `require("x")` with a string literal argument: the specifier.
pub fn require_spec<'a>(e: &'a Expression<'_>) -> Option<&'a str> {
    let Expression::CallExpression(c) = e.get_inner_expression() else {
        return None;
    };
    let Expression::Identifier(id) = c.callee.get_inner_expression() else {
        return None;
    };
    if id.name.as_str() != "require" || c.arguments.len() != 1 {
        return None;
    }
    match c.arguments[0].as_expression()?.get_inner_expression() {
        Expression::StringLiteral(s) => Some(s.value.as_str()),
        _ => None,
    }
}

impl Builder<'_> {
    pub(crate) fn modules_for(&self, spec: &str) -> Vec<ExportKey> {
        module_candidates(self.ctx, self.path.as_str(), spec)
    }

    /// An `imports` reference from the artifact to `candidates`.
    pub(crate) fn import_ref(&mut self, candidates: Vec<ExportKey>, span: Span) {
        if candidates.is_empty() {
            return;
        }
        let from = self.artifact_id;
        self.push_ref(
            from,
            "imports",
            Some(span),
            candidates,
            Confidence::Extracted,
        );
    }

    pub(crate) fn push_ref(
        &mut self,
        from: katalogr_model::Id,
        kind: &str,
        span: Option<Span>,
        candidates: Vec<ExportKey>,
        confidence: Confidence,
    ) {
        self.refs.push(katalogr_model::OutRef {
            from,
            kind: LinkKind(kind.to_string()),
            span,
            candidates,
            confidence,
        });
    }

    /// `import … from "x"`: one reference to the module and one per named specifier.
    pub(crate) fn import_declaration(&mut self, d: &ImportDeclaration<'_>) {
        let modules = self.modules_for(d.source.value.as_str());
        let span = self.lines.span(d.span.start, d.span.end);
        self.import_ref(modules.clone(), span);
        for spec in d.specifiers.iter().flatten() {
            match spec {
                ImportDeclarationSpecifier::ImportSpecifier(s) => {
                    let imported = module_export_name(&s.imported).to_string();
                    let span = self.lines.span(s.span.start, s.span.end);
                    self.import_ref(export_candidates(&modules, &imported), span);
                    self.imports.insert(
                        s.local.name.as_str().to_string(),
                        Binding::Named {
                            modules: modules.clone(),
                            name: imported,
                        },
                    );
                }
                ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                    let span = self.lines.span(s.span.start, s.span.end);
                    self.import_ref(export_candidates(&modules, "default"), span);
                    self.imports.insert(
                        s.local.name.as_str().to_string(),
                        Binding::Named {
                            modules: modules.clone(),
                            name: "default".to_string(),
                        },
                    );
                }
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                    self.imports.insert(
                        s.local.name.as_str().to_string(),
                        Binding::Namespace {
                            modules: modules.clone(),
                        },
                    );
                }
            }
        }
    }

    /// `const x = require("m")`, `const { a, b: c } = require("m")`, `const f = require("m").f`:
    /// bindings and named-export references. (The reference to the module itself comes from
    /// the call walk.)
    pub(crate) fn require_bindings(&mut self, v: &VariableDeclaration<'_>) {
        for d in &v.declarations {
            let Some(init) = &d.init else { continue };
            let inner = init.get_inner_expression();
            if let Some(spec) = require_spec(inner) {
                let modules = self.modules_for(spec);
                match &d.id {
                    BindingPattern::BindingIdentifier(id) => {
                        self.imports
                            .insert(id.name.as_str().to_string(), Binding::Namespace { modules });
                    }
                    BindingPattern::ObjectPattern(o) => {
                        for p in &o.properties {
                            let (Some(name), BindingPattern::BindingIdentifier(local)) =
                                (static_key(&p.key), &p.value)
                            else {
                                continue;
                            };
                            let span = self.lines.span(p.span.start, p.span.end);
                            self.import_ref(export_candidates(&modules, &name), span);
                            self.imports.insert(
                                local.name.as_str().to_string(),
                                Binding::Named {
                                    modules: modules.clone(),
                                    name,
                                },
                            );
                        }
                    }
                    _ => {}
                }
            } else if let Expression::StaticMemberExpression(m) = inner
                && let Some(spec) = require_spec(&m.object)
                && let BindingPattern::BindingIdentifier(id) = &d.id
            {
                let modules = self.modules_for(spec);
                let name = m.property.name.as_str().to_string();
                let span = self.lines.span(m.span.start, m.span.end);
                self.import_ref(export_candidates(&modules, &name), span);
                self.imports.insert(
                    id.name.as_str().to_string(),
                    Binding::Named { modules, name },
                );
            }
        }
    }
}

/// The name of a statically known property key (`#x` for private names).
pub fn static_key(k: &PropertyKey<'_>) -> Option<String> {
    match k {
        PropertyKey::PrivateIdentifier(p) => Some(format!("#{}", p.name.as_str())),
        other => other.static_name().map(|c| c.into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(v: &[ExportKey]) -> Vec<String> {
        v.iter()
            .map(|k| format!("{}:{}", k.namespace, k.key))
            .collect()
    }

    #[test]
    fn relative_candidates_in_order() {
        let c = module_candidates(&TsContext::default(), "src/a/b.ts", "../util");
        let k = keys(&c);
        assert_eq!(k[0], "ts-file:src/util");
        assert_eq!(k[1], "ts-file:src/util.ts");
        assert_eq!(k.last().unwrap(), "ts-file:src/util/index.js");
        assert!(module_candidates(&TsContext::default(), "a.ts", "../x").is_empty());
    }

    #[test]
    fn package_names() {
        assert_eq!(package_name_of("react"), ("react", ""));
        assert_eq!(package_name_of("@s/p/sub/x"), ("@s/p", "sub/x"));
        assert_eq!(package_name_of("lodash/fp"), ("lodash", "fp"));
        assert_eq!(match_pattern("@a/*", "@a/b/c"), Some("b/c"));
        assert_eq!(match_pattern("x", "y"), None);
    }
}
