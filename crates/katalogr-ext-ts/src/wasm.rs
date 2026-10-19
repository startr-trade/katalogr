//! The TypeScript side of the WASM bridge (FEAT-016 sections 6.3 and 6.4): imports of a wasm
//! package become `consumes` bindings to the `wasm_export` contract the Rust export provides,
//! and committed glue files `declare` the same contracts. The convention (package keys, glue
//! names) lives in `katalogr-ext-wasm`; this module only reads syntax.

use crate::items::Builder;
use crate::modules::{NS_FILE, require_spec};
use katalogr_ext_wasm::pkg::{contract, is_glue_only};
use katalogr_model::{Binding, Confidence, ContractRole, ExportKey, Fact, Id, Name, Span};
use oxc_ast::ast::{BindingPattern, Expression, StaticMemberExpression, VariableDeclarator};

/// What a local name imported from a wasm package refers to.
#[derive(Clone, Debug)]
pub(crate) enum WasmImport {
    /// `import { E as local }`, `import local from` (E = `default`), `const { E: local } = require(..)`.
    Named { package: String, export: String },
    /// `import * as local`, `const local = require(..)`, `const local = await import(..)`.
    Namespace { package: String },
}

/// `a.b.c` as `("a", ["b", "c"])` for static members only.
fn member_chain(e: &Expression<'_>) -> Option<(String, Vec<String>)> {
    match e.get_inner_expression() {
        Expression::Identifier(id) => Some((id.name.as_str().to_string(), Vec::new())),
        Expression::StaticMemberExpression(m) => {
            let (root, mut rest) = member_chain(&m.object)?;
            rest.push(m.property.name.as_str().to_string());
            Some((root, rest))
        }
        _ => None,
    }
}

impl Builder<'_> {
    /// Classify an import of `spec` (already resolved to `modules`); `Some(package key)` when it
    /// is a wasm package and this file is not itself glue.
    pub(crate) fn wasm_package_of(&self, spec: &str, modules: &[ExportKey]) -> Option<String> {
        if self.ctx.wasm.is_empty() || self.glue.is_some() {
            return None;
        }
        let files: Vec<&str> = modules
            .iter()
            .filter(|k| k.namespace == NS_FILE)
            .map(|k| k.key.as_str())
            .collect();
        self.ctx
            .wasm
            .for_import(spec, &files)
            .map(|p| p.key.clone())
    }

    fn bind(
        &mut self,
        symbol: Id,
        c: &str,
        p: &str,
        role: ContractRole,
        span: Span,
        conf: Confidence,
    ) {
        self.bindings.push(Binding {
            symbol,
            contract: contract(c, p),
            role,
            span: Some(span),
            confidence: conf,
            origin: None,
        });
    }

    /// Record `local` as a wasm import, and bind the artifact `consumes` for a named import
    /// (skipping glue-only names).
    pub(crate) fn wasm_import(&mut self, local: &str, import: WasmImport, span: Span) {
        if let WasmImport::Named { package, export } = &import
            && !is_glue_only(export)
        {
            let artifact = self.artifact_id;
            self.bind(
                artifact,
                package,
                export,
                ContractRole::Consumes,
                span,
                Confidence::Extracted,
            );
        }
        self.wasm_imports.insert(local.to_string(), import);
    }

    /// Bind `from` `consumes` for a call, `new`, or member use whose callee is a wasm import or
    /// a tracked instance. Returns true when it bound something.
    pub(crate) fn wasm_use(&mut self, from: Id, callee: &Expression<'_>, span: Span) -> bool {
        if self.wasm_imports.is_empty() {
            return false;
        }
        match member_chain(callee) {
            Some((root, rest)) => self.wasm_use_chain(from, root, rest, span),
            None => false,
        }
    }

    /// The same for a static member read that is not (necessarily) a callee.
    pub(crate) fn wasm_use_member(
        &mut self,
        from: Id,
        m: &StaticMemberExpression<'_>,
        span: Span,
    ) -> bool {
        if self.wasm_imports.is_empty() {
            return false;
        }
        let Some((root, mut rest)) = member_chain(&m.object) else {
            return false;
        };
        rest.push(m.property.name.as_str().to_string());
        self.wasm_use_chain(from, root, rest, span)
    }

    fn wasm_use_chain(&mut self, from: Id, root: String, rest: Vec<String>, span: Span) -> bool {
        let (package, path, conf) = match self.wasm_imports.get(&root) {
            Some(WasmImport::Named { package, export }) => {
                let mut segs = vec![export.clone()];
                segs.extend(rest.first().cloned());
                (package.clone(), segs, Confidence::Extracted)
            }
            Some(WasmImport::Namespace { package }) => {
                if rest.is_empty() {
                    return false;
                }
                (
                    package.clone(),
                    rest.iter().take(2).cloned().collect(),
                    Confidence::Extracted,
                )
            }
            None => match self.wasm_instances.get(&root) {
                Some((package, class)) if rest.len() == 1 => (
                    package.clone(),
                    vec![class.clone(), rest[0].clone()],
                    Confidence::Inferred,
                ),
                _ => return false,
            },
        };
        if path.iter().any(|s| is_glue_only(s)) {
            return false;
        }
        self.bind(
            from,
            &package,
            &path.join("."),
            ContractRole::Consumes,
            span,
            conf,
        );
        true
    }

    /// Track `const|let x = new C(..)` where `C` is a named wasm import, for the current walk.
    pub(crate) fn wasm_track_instance(&mut self, decl: &VariableDeclarator<'_>) {
        if self.wasm_imports.is_empty() {
            return;
        }
        let BindingPattern::BindingIdentifier(id) = &decl.id else {
            return;
        };
        let Some(init) = &decl.init else { return };
        let mut e = init.get_inner_expression();
        while let Expression::AwaitExpression(a) = e {
            e = a.argument.get_inner_expression();
        }
        let Expression::NewExpression(n) = e else {
            return;
        };
        let Expression::Identifier(callee) = n.callee.get_inner_expression() else {
            return;
        };
        if let Some(WasmImport::Named { package, export }) =
            self.wasm_imports.get(callee.name.as_str())
        {
            let entry = (package.clone(), export.clone());
            self.wasm_instances
                .insert(id.name.as_str().to_string(), entry);
        }
    }

    /// Forget tracked instances (called at the start of each pending item's walk).
    pub(crate) fn wasm_reset_instances(&mut self) {
        self.wasm_instances.clear();
    }

    /// `const n = require("x")`, `const { E: l } = require("x")`, `const f = require("x").E` and
    /// `const n = await import("x")` where `x` is a wasm package.
    pub(crate) fn wasm_declarator_imports(&mut self, d: &VariableDeclarator<'_>) {
        if self.ctx.wasm.is_empty() || self.glue.is_some() {
            return;
        }
        let Some(init) = &d.init else { return };
        let inner = init.get_inner_expression();
        let span = self.span(d.span);
        if let Some(spec) = require_spec(inner) {
            let modules = self.modules_for(spec);
            let Some(package) = self.wasm_package_of(spec, &modules) else {
                return;
            };
            match &d.id {
                BindingPattern::BindingIdentifier(id) => {
                    self.wasm_import(id.name.as_str(), WasmImport::Namespace { package }, span);
                }
                BindingPattern::ObjectPattern(o) => {
                    for p in &o.properties {
                        let (Some(name), BindingPattern::BindingIdentifier(local)) =
                            (crate::modules::static_key(&p.key), &p.value)
                        else {
                            continue;
                        };
                        let span = self.span(p.span);
                        self.wasm_import(
                            local.name.as_str(),
                            WasmImport::Named {
                                package: package.clone(),
                                export: name,
                            },
                            span,
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
            if let Some(package) = self.wasm_package_of(spec, &modules) {
                let export = m.property.name.as_str().to_string();
                let span = self.span(m.span);
                self.wasm_import(
                    id.name.as_str(),
                    WasmImport::Named { package, export },
                    span,
                );
            }
        } else {
            let mut e = inner;
            while let Expression::AwaitExpression(a) = e {
                e = a.argument.get_inner_expression();
            }
            if let Expression::ImportExpression(i) = e
                && let Expression::StringLiteral(s) = i.source.get_inner_expression()
                && let BindingPattern::BindingIdentifier(id) = &d.id
            {
                let spec = s.value.as_str();
                let modules = self.modules_for(spec);
                if let Some(package) = self.wasm_package_of(spec, &modules) {
                    self.wasm_import(id.name.as_str(), WasmImport::Namespace { package }, span);
                }
            }
        }
    }

    /// When this file is glue, bind its exported symbols `declares` (section 6.4).
    pub(crate) fn declare_glue(&mut self) {
        let Some(package) = self.glue.clone() else {
            return;
        };
        let exported = |s: &katalogr_model::SymbolNode| {
            matches!(s.facts.get("exported"), Some(Fact::Bool(true)))
        };
        let mut found: Vec<(Id, String, Span)> = Vec::new();
        for s in &self.symbols {
            let segs = &s.name.segments;
            let head = match segs.as_slice() {
                [h] if matches!(
                    s.kind.as_str(),
                    "fn" | "class" | "const" | "enum" | "type" | "interface"
                ) && exported(s) =>
                {
                    if matches!(s.kind.as_str(), "type" | "interface") && h.name.starts_with("Init")
                    {
                        continue;
                    }
                    h.name.clone()
                }
                [c, m] if c.kind == "class" && m.kind == "method" => {
                    let class_exported = self
                        .symbols
                        .iter()
                        .any(|x| x.kind == "class" && x.name.segments == segs[..1] && exported(x));
                    if !class_exported || m.name.starts_with('#') {
                        continue;
                    }
                    if m.name == "constructor" {
                        c.name.clone()
                    } else {
                        format!("{}.{}", c.name, m.name)
                    }
                }
                _ => continue,
            };
            if head.split('.').any(is_glue_only) {
                continue;
            }
            found.push((s.name.id(), head, s.span));
        }
        for (id, path, span) in found {
            self.bind(
                id,
                &package,
                &path,
                ContractRole::Declares,
                span,
                Confidence::Extracted,
            );
        }
    }

    /// The bindings of the file, sorted by (contract, role, symbol) and deduplicated on that
    /// triple; the first span is kept.
    pub(crate) fn take_bindings(&mut self) -> Vec<Binding> {
        let mut b = std::mem::take(&mut self.bindings);
        b.sort_by_cached_key(|x| (x.contract.canonical(), x.role, x.symbol));
        b.dedup_by(|a, b| a.contract == b.contract && a.role == b.role && a.symbol == b.symbol);
        b
    }
}
