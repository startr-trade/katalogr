//! Items of one file: functions, classes, methods, interfaces, type aliases, enums and
//! exported constants holding functions, plus the file's exports and re-exports
//! (FEAT-006 section 4).

use crate::context::TsContext;
use crate::lines::Lines;
use crate::modules::{
    NS_EXPORT, NS_FILE, NS_MEMBER, export_candidates, export_name, key, module_export_name,
    static_key,
};
use crate::roots::{self, RootCtx};
use katalogr_model::annotate;
use katalogr_model::insight::FACT_ROOT;
use katalogr_model::{
    ArtifactName, Confidence, Export, ExportKey, Fact, Id, Link, LinkKind, Name, OutRef,
    Provenance, RelPath, Segment, Span, SymbolName, SymbolNode,
};
use oxc_ast::ast::{
    ArrowFunctionExpression, AssignmentOperator, Class, ClassElement, Declaration,
    ExportDefaultDeclarationKind, Expression, ExpressionStatement, Function, MemberExpression,
    ModuleDeclaration, ObjectPropertyKind, PropertyKind, Statement, VariableDeclarator,
};
use oxc_span::GetSpan;
use std::collections::{BTreeMap, HashMap};

/// What a local name stands for when it was imported.
#[derive(Clone, Debug)]
pub enum Binding {
    Named {
        modules: Vec<ExportKey>,
        name: String,
    },
    Namespace {
        modules: Vec<ExportKey>,
    },
}

/// Code whose calls are attributed to a symbol, collected during the declaration pass and
/// walked afterwards, so every local name is known first.
pub enum Pending<'p, 'a> {
    Func(Id, &'p Function<'a>),
    Arrow(Id, &'p ArrowFunctionExpression<'a>),
    ClassElement(Id, &'p ClassElement<'a>),
    /// Anything else at module level; calls belong to the artifact.
    Statement(&'p Statement<'a>),
    Declarator(&'p VariableDeclarator<'a>),
    Expression(&'p Expression<'a>),
    /// A statement inside a test suite callback; calls belong to the suite symbol (FEAT-063).
    Within(Id, &'p Statement<'a>),
}

pub struct Builder<'s> {
    pub(crate) ctx: &'s TsContext,
    pub(crate) path: &'s RelPath,
    pub(crate) text: &'s str,
    pub(crate) lines: Lines<'s>,
    pub(crate) artifact: ArtifactName,
    pub(crate) artifact_id: Id,
    pub(crate) provenance: Provenance,
    pub(crate) symbols: Vec<SymbolNode>,
    pub(crate) links: Vec<Link>,
    pub(crate) refs: Vec<OutRef>,
    pub(crate) exports: Vec<Export>,
    pub(crate) imports: HashMap<String, Binding>,
    /// Top-level functions, classes and constants holding functions, by name.
    pub(crate) callable: HashMap<String, Id>,
    /// Every top-level named symbol, by name (first declaration wins).
    declared: HashMap<String, Id>,
    ordinals: HashMap<Vec<Segment>, u32>,
    /// `export { local as exported }` without a source, resolved after the declarations.
    export_locals: Vec<(String, String, Span)>,
    /// Locals imported from a wasm package (FEAT-016).
    pub(crate) wasm_imports: HashMap<String, crate::wasm::WasmImport>,
    /// `const x = new C()` locals of a wasm class, per pending item: name to (package key, class).
    pub(crate) wasm_instances: HashMap<String, (String, String)>,
    pub(crate) bindings: Vec<katalogr_model::Binding>,
    /// The package key when this file is generated wasm glue.
    pub(crate) glue: Option<String>,
    /// Inside the methods of a class that implements or extends something.
    in_derived_class: bool,
}

fn is_function(e: &Expression<'_>) -> bool {
    matches!(
        e.get_inner_expression(),
        Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
    )
}

impl<'s> Builder<'s> {
    pub fn new(
        ctx: &'s TsContext,
        repo: &str,
        path: &'s RelPath,
        revision: &str,
        text: &'s str,
    ) -> Builder<'s> {
        let artifact = ArtifactName {
            repo: repo.to_string(),
            path: path.clone(),
        };
        let artifact_id = artifact.id();
        Builder {
            ctx,
            path,
            text,
            lines: Lines::new(text),
            artifact,
            artifact_id,
            provenance: Provenance {
                repo: repo.to_string(),
                path: path.clone(),
                revision: revision.to_string(),
            },
            symbols: Vec::new(),
            links: Vec::new(),
            refs: Vec::new(),
            exports: Vec::new(),
            imports: HashMap::new(),
            callable: HashMap::new(),
            declared: HashMap::new(),
            ordinals: HashMap::new(),
            export_locals: Vec::new(),
            wasm_imports: HashMap::new(),
            wasm_instances: HashMap::new(),
            bindings: Vec::new(),
            glue: ctx.wasm.glue_of(path.as_str()).map(|p| p.key.clone()),
            in_derived_class: false,
        }
    }

    pub(crate) fn span(&self, s: oxc_span::Span) -> Span {
        self.lines.span(s.start, s.end)
    }

    /// One-line, whitespace-normalised source text of `start..end`.
    fn signature(&self, start: u32, end: u32) -> String {
        let raw = self.text.get(start as usize..end as usize).unwrap_or("");
        let one = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        one.trim_end_matches("=>").trim_end().to_string()
    }

    /// The first paragraph of the JSDoc comment (`/** ... */`) that ends right before the line
    /// `line` (1-based), whitespace normalised to one line.
    fn doc_before(&self, line: u32) -> Option<String> {
        let start = self.lines.line_start(line);
        let before = self.text.get(..start)?.trim_end();
        let inner = before.strip_suffix("*/")?;
        let open = inner.rfind("/*")?;
        let body = inner[open..].strip_prefix("/**")?;
        if body.starts_with('*') {
            return None;
        }
        let mut lines: Vec<&str> = Vec::new();
        for l in body.lines() {
            let l = l.trim();
            lines.push(l.strip_prefix('*').unwrap_or(l).trim());
        }
        let paragraph: Vec<&str> = lines
            .into_iter()
            .skip_while(|l| l.is_empty() || l.starts_with('@'))
            .take_while(|l| !l.is_empty() && !l.starts_with('@'))
            .collect();
        let doc = paragraph.join(" ");
        (!doc.is_empty()).then_some(doc)
    }

    pub(crate) fn add_symbol(
        &mut self,
        segments: Vec<Segment>,
        kind: &str,
        span: Span,
        mut facts: BTreeMap<String, Fact>,
    ) -> Id {
        if let Some(doc) = self.doc_before(span.start_line) {
            facts.insert("doc".to_string(), Fact::Str(doc));
        }
        let reason = roots::root_reason(&RootCtx {
            kind,
            test_file: roots::is_test_path(self.path.as_str()),
            exported: matches!(facts.get("exported"), Some(Fact::Bool(true))),
            in_derived_class: self.in_derived_class,
        });
        if let Some(r) = reason {
            facts.insert(FACT_ROOT.to_string(), Fact::Str(r.to_string()));
        }
        let n = self.ordinals.entry(segments.clone()).or_insert(0);
        *n += 1;
        let name = SymbolName {
            artifact: self.artifact.clone(),
            segments,
            ordinal: *n,
        };
        let id = name.id();
        self.symbols.push(SymbolNode {
            name,
            kind: kind.to_string(),
            span,
            facts,
        });
        self.annotations(id, span);
        id
    }

    /// FEAT-019 T6: item annotations (`FEAT-001 R-1`) in the comment lines directly above the
    /// item become `annotated-by` references from the symbol.
    fn annotations(&mut self, id: Id, span: Span) {
        let lines: Vec<&str> = self.text.lines().collect();
        let mut i = (span.start_line as usize).saturating_sub(1);
        let mut found: Vec<(u32, String)> = Vec::new();
        while i > 0 {
            let t = lines.get(i - 1).map_or("", |l| l.trim());
            let comment = t.starts_with("//")
                || t.starts_with("/*")
                || t.starts_with('*')
                || t.ends_with("*/");
            if !comment {
                break;
            }
            found.push((i as u32, t.to_string()));
            i -= 1;
        }
        found.reverse();
        for (line, text) in found {
            let at = Span {
                start_line: line,
                start_col: 1,
                end_line: line,
                end_col: text.chars().count().max(1) as u32,
            };
            for r in annotate::scan(&text) {
                let keys = annotate::keys(&r);
                let dup = self
                    .refs
                    .iter()
                    .any(|x| x.from == id && x.kind.0 == "annotated-by" && x.candidates == keys);
                if !dup {
                    self.refs.push(OutRef {
                        from: id,
                        kind: LinkKind("annotated-by".to_string()),
                        span: Some(at),
                        candidates: keys,
                        confidence: Confidence::Extracted,
                        origin: None,
                    });
                }
            }
        }
    }

    pub(crate) fn export(&mut self, namespace: &str, k: String, target: Id) {
        self.exports.push(Export {
            key: key(namespace, k),
            target,
        });
    }

    pub(crate) fn contains(&mut self, from: Id, to: Id, span: Span) {
        self.links.push(Link {
            from,
            to,
            kind: LinkKind("contains".to_string()),
            confidence: Confidence::Extracted,
            span: Some(span),
            provenance: self.provenance.clone(),
        });
    }

    pub(crate) fn seg(kind: &str, name: &str) -> Segment {
        Segment {
            kind: kind.to_string(),
            name: name.to_string(),
        }
    }

    fn base_facts(exported: bool, default: bool, is_async: bool) -> BTreeMap<String, Fact> {
        let mut f = BTreeMap::new();
        f.insert("exported".to_string(), Fact::Bool(exported));
        if default {
            f.insert("default".to_string(), Fact::Bool(true));
        }
        if is_async {
            f.insert("async".to_string(), Fact::Bool(true));
        }
        f
    }

    fn register(&mut self, name: &str, id: Id, callable: bool, exported: bool, default: bool) {
        self.declared.entry(name.to_string()).or_insert(id);
        if callable {
            self.callable.entry(name.to_string()).or_insert(id);
        }
        if exported {
            let export_as = if default { "default" } else { name };
            self.export(NS_EXPORT, export_name(self.path.as_str(), export_as), id);
        }
    }

    /// Declare the item(s) of one top-level statement.
    pub fn declare_statement<'p, 'a>(
        &mut self,
        stmt: &'p Statement<'a>,
        pending: &mut Vec<Pending<'p, 'a>>,
    ) {
        if let Some(d) = stmt.as_declaration() {
            if !self.declare(d, GetSpan::span(stmt), false, pending) {
                pending.push(Pending::Statement(stmt));
            }
            return;
        }
        if let Statement::ExpressionStatement(es) = stmt
            && self.commonjs_export(es, pending)
        {
            return;
        }
        let Some(m) = stmt.as_module_declaration() else {
            pending.push(Pending::Statement(stmt));
            return;
        };
        match m {
            ModuleDeclaration::ExportDeclaration(e) => {
                self.declare(&e.declaration, e.span, true, pending);
            }
            ModuleDeclaration::ExportNamedDeclaration(e) => {
                for s in &e.specifiers {
                    self.export_locals.push((
                        module_export_name(&s.local).to_string(),
                        module_export_name(&s.exported).to_string(),
                        self.span(s.span),
                    ));
                }
            }
            ModuleDeclaration::ExportFromDeclaration(e) => {
                let modules = self.modules_for(e.source.value.as_str());
                for s in &e.specifiers {
                    let local = module_export_name(&s.local).to_string();
                    let exported = module_export_name(&s.exported).to_string();
                    let span = self.span(s.span);
                    let cands = export_candidates(&modules, &local);
                    self.reexport(&exported, Some(exported.clone()), cands, span);
                }
            }
            ModuleDeclaration::ExportAllDeclaration(e) => {
                let modules = self.modules_for(e.source.value.as_str());
                let span = self.span(e.span);
                let exported = e
                    .exported
                    .as_ref()
                    .map(|n| module_export_name(n).to_string());
                let name = exported.clone().unwrap_or_else(|| "*".to_string());
                self.reexport(&name, exported, modules, span);
            }
            ModuleDeclaration::ExportDefaultDeclaration(e) => {
                let span = self.span(e.span);
                match &e.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                        self.declare_function(f, span, true, true, pending);
                    }
                    ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                        self.declare_class(c, span, true, true, pending);
                    }
                    ExportDefaultDeclarationKind::TSInterfaceDeclaration(i) => {
                        self.simple(i.id.name.as_str(), "interface", span, true, true);
                    }
                    other => {
                        let expr = other.to_expression();
                        let inner = expr.get_inner_expression();
                        match inner {
                            Expression::Identifier(id) => {
                                self.export_locals.push((
                                    id.name.as_str().to_string(),
                                    "default".to_string(),
                                    span,
                                ));
                            }
                            Expression::ArrowFunctionExpression(a) => {
                                let mut facts = Self::base_facts(true, true, a.r#async);
                                facts.insert(
                                    "signature".into(),
                                    Fact::Str(self.signature(a.span.start, body_start(a))),
                                );
                                let id = self.add_symbol(
                                    vec![Self::seg("fn", "default")],
                                    "fn",
                                    span,
                                    facts,
                                );
                                self.register("default", id, true, true, true);
                                pending.push(Pending::Arrow(id, a));
                            }
                            Expression::FunctionExpression(f) => {
                                self.declare_function(f, span, true, true, pending);
                            }
                            _ => pending.push(Pending::Expression(expr)),
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// `exports.x = v`, `module.exports.x = v` and `module.exports = { a, b: c }` at the top
    /// level: each member is an export of the file. Returns false for any other statement.
    fn commonjs_export<'p, 'a>(
        &mut self,
        es: &'p ExpressionStatement<'a>,
        pending: &mut Vec<Pending<'p, 'a>>,
    ) -> bool {
        let Expression::AssignmentExpression(a) = es.expression.get_inner_expression() else {
            return false;
        };
        if a.operator != AssignmentOperator::Assign {
            return false;
        }
        let Some(MemberExpression::StaticMemberExpression(m)) = a.left.as_member_expression()
        else {
            return false;
        };
        let prop = m.property.name.as_str();
        let object = m.object.get_inner_expression();
        let is_ident = |e: &Expression<'_>, n: &str| matches!(e.get_inner_expression(), Expression::Identifier(i) if i.name.as_str() == n);
        let is_module_exports = matches!(
            object,
            Expression::StaticMemberExpression(o)
                if is_ident(&o.object, "module") && o.property.name.as_str() == "exports"
        );
        if is_ident(object, "exports") || is_module_exports {
            let span = self.span(es.span);
            self.commonjs_member(prop, &a.right, span, pending);
            return true;
        }
        if is_ident(object, "module")
            && prop == "exports"
            && let Expression::ObjectExpression(o) = a.right.get_inner_expression()
        {
            for p in &o.properties {
                let ObjectPropertyKind::ObjectProperty(p) = p else {
                    continue;
                };
                if p.kind != PropertyKind::Init || p.computed {
                    continue;
                }
                let Some(name) = static_key(&p.key) else {
                    continue;
                };
                let span = self.span(p.span);
                self.commonjs_member(&name, &p.value, span, pending);
            }
            return true;
        }
        false
    }

    /// One CommonJS export `name = value`: a function value becomes an exported `fn` symbol,
    /// a name of a local binding exports that binding, anything else an exported `const`.
    fn commonjs_member<'p, 'a>(
        &mut self,
        name: &str,
        value: &'p Expression<'a>,
        span: Span,
        pending: &mut Vec<Pending<'p, 'a>>,
    ) {
        let inner = value.get_inner_expression();
        match inner {
            Expression::Identifier(id) => {
                self.export_locals
                    .push((id.name.as_str().to_string(), name.to_string(), span));
            }
            Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_) => {
                let (is_async, sig_end) = match inner {
                    Expression::ArrowFunctionExpression(a) => (a.r#async, body_start(a)),
                    Expression::FunctionExpression(f) => (
                        f.r#async,
                        f.body.as_ref().map_or(f.span.end, |b| b.span.start),
                    ),
                    _ => unreachable!(),
                };
                let mut facts = Self::base_facts(true, false, is_async);
                facts.insert(
                    "signature".into(),
                    Fact::Str(self.signature(inner_start(inner), sig_end)),
                );
                let id = self.add_symbol(vec![Self::seg("fn", name)], "fn", span, facts);
                self.register(name, id, true, true, false);
                match inner {
                    Expression::ArrowFunctionExpression(a) => pending.push(Pending::Arrow(id, a)),
                    Expression::FunctionExpression(f) => pending.push(Pending::Func(id, f)),
                    _ => {}
                }
            }
            _ => {
                let facts = Self::base_facts(true, false, false);
                let id = self.add_symbol(vec![Self::seg("const", name)], "const", span, facts);
                self.register(name, id, false, true, false);
                pending.push(Pending::Expression(value));
            }
        }
    }

    /// Returns false when the declaration contributed no item, so the statement should be
    /// walked for calls from the artifact.
    fn declare<'p, 'a>(
        &mut self,
        d: &'p Declaration<'a>,
        range: oxc_span::Span,
        exported: bool,
        pending: &mut Vec<Pending<'p, 'a>>,
    ) -> bool {
        let span = self.span(range);
        match d {
            Declaration::FunctionDeclaration(f) => {
                self.declare_function(f, span, exported, false, pending)
            }
            Declaration::ClassDeclaration(c) => {
                self.declare_class(c, span, exported, false, pending)
            }
            Declaration::TSInterfaceDeclaration(i) => {
                self.simple(i.id.name.as_str(), "interface", span, exported, false)
            }
            Declaration::TSTypeAliasDeclaration(t) => {
                self.simple(t.id.name.as_str(), "type", span, exported, false)
            }
            Declaration::TSEnumDeclaration(e) => {
                self.simple(e.id.name.as_str(), "enum", span, exported, false)
            }
            Declaration::VariableDeclaration(v) => {
                let mut any = false;
                for decl in &v.declarations {
                    let named = decl.id.get_binding_identifier();
                    match (named, &decl.init) {
                        (Some(id), Some(init)) if exported && is_function(init) => {
                            let inner = init.get_inner_expression();
                            let (is_async, sig_end) = match inner {
                                Expression::ArrowFunctionExpression(a) => {
                                    (a.r#async, body_start(a))
                                }
                                Expression::FunctionExpression(f) => (
                                    f.r#async,
                                    f.body.as_ref().map_or(f.span.end, |b| b.span.start),
                                ),
                                _ => unreachable!(),
                            };
                            let mut facts = Self::base_facts(true, false, is_async);
                            facts.insert(
                                "signature".into(),
                                Fact::Str(self.signature(inner_start(inner), sig_end)),
                            );
                            let name = id.name.as_str().to_string();
                            let sid = self.add_symbol(
                                vec![Self::seg("const", &name)],
                                "const",
                                span,
                                facts,
                            );
                            self.register(&name, sid, true, true, false);
                            match inner {
                                Expression::ArrowFunctionExpression(a) => {
                                    pending.push(Pending::Arrow(sid, a));
                                }
                                Expression::FunctionExpression(f) => {
                                    pending.push(Pending::Func(sid, f));
                                }
                                _ => {}
                            }
                            any = true;
                        }
                        _ => pending.push(Pending::Declarator(decl)),
                    }
                }
                any
            }
            _ => false,
        }
    }

    fn simple(
        &mut self,
        name: &str,
        kind: &str,
        span: Span,
        exported: bool,
        default: bool,
    ) -> bool {
        let facts = Self::base_facts(exported, default, false);
        let id = self.add_symbol(vec![Self::seg(kind, name)], kind, span, facts);
        self.register(name, id, false, exported, default);
        true
    }

    fn declare_function<'p, 'a>(
        &mut self,
        f: &'p Function<'a>,
        span: Span,
        exported: bool,
        default: bool,
        pending: &mut Vec<Pending<'p, 'a>>,
    ) -> bool {
        let name =
            f.id.as_ref()
                .map_or("default", |i| i.name.as_str())
                .to_string();
        // Overload signatures and `declare function`: the first declaration stands for all.
        if self.callable.contains_key(&name) && f.body.is_none() {
            return true;
        }
        let mut facts = Self::base_facts(exported, default, f.r#async);
        let end = f.body.as_ref().map_or(f.span.end, |b| b.span.start);
        facts.insert(
            "signature".into(),
            Fact::Str(self.signature(f.span.start, end)),
        );
        let id = self.add_symbol(vec![Self::seg("fn", &name)], "fn", span, facts);
        self.register(&name, id, true, exported, default);
        if f.body.is_some() {
            pending.push(Pending::Func(id, f));
        }
        true
    }

    fn declare_class<'p, 'a>(
        &mut self,
        c: &'p Class<'a>,
        span: Span,
        exported: bool,
        default: bool,
        pending: &mut Vec<Pending<'p, 'a>>,
    ) -> bool {
        let name =
            c.id.as_ref()
                .map_or("default", |i| i.name.as_str())
                .to_string();
        let mut facts = Self::base_facts(exported, default, false);
        facts.insert(
            "signature".into(),
            Fact::Str(self.signature(c.span.start, c.body.span.start)),
        );
        if c.r#abstract {
            facts.insert("abstract".into(), Fact::Bool(true));
        }
        self.decorators(&mut facts, &c.decorators);
        let cid = self.add_symbol(vec![Self::seg("class", &name)], "class", span, facts);
        self.register(&name, cid, true, exported, default);
        let saved_derived = std::mem::replace(
            &mut self.in_derived_class,
            !c.implements.is_empty() || c.heritage.is_some(),
        );
        for el in &c.body.body {
            match el {
                ClassElement::MethodDefinition(m) => {
                    let Some(mname) = static_key(&m.key) else {
                        continue;
                    };
                    let mut facts = Self::base_facts(false, false, m.value.r#async);
                    facts.remove("exported");
                    facts.insert(
                        "signature".into(),
                        Fact::Str(self.signature(
                            m.value.span.start.min(m.key.span().start),
                            m.value.body.as_ref().map_or(m.span.end, |b| b.span.start),
                        )),
                    );
                    if m.r#static {
                        facts.insert("static".into(), Fact::Bool(true));
                    }
                    match m.kind {
                        oxc_ast::ast::MethodDefinitionKind::Get => {
                            facts.insert("accessor".into(), Fact::Str("get".into()));
                        }
                        oxc_ast::ast::MethodDefinitionKind::Set => {
                            facts.insert("accessor".into(), Fact::Str("set".into()));
                        }
                        _ => {}
                    }
                    self.decorators(&mut facts, &m.decorators);
                    let mspan = self.span(m.span);
                    let mid = self.add_symbol(
                        vec![Self::seg("class", &name), Self::seg("method", &mname)],
                        "method",
                        mspan,
                        facts,
                    );
                    self.contains(cid, mid, mspan);
                    if !matches!(m.kind, oxc_ast::ast::MethodDefinitionKind::Constructor) {
                        self.export(NS_MEMBER, mname, mid);
                    }
                    if m.value.body.is_some() {
                        pending.push(Pending::Func(mid, &m.value));
                    }
                }
                ClassElement::PropertyDefinition(_)
                | ClassElement::AccessorProperty(_)
                | ClassElement::StaticBlock(_) => pending.push(Pending::ClassElement(cid, el)),
                _ => {}
            }
        }
        self.in_derived_class = saved_derived;
        true
    }

    fn decorators(
        &self,
        facts: &mut BTreeMap<String, Fact>,
        decorators: &[oxc_ast::ast::Decorator<'_>],
    ) {
        if decorators.is_empty() {
            return;
        }
        let list = decorators
            .iter()
            .map(|d| {
                let t = self.text.get(d.span.start as usize..d.span.end as usize);
                Fact::Str(
                    t.unwrap_or("")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            })
            .collect();
        facts.insert("decorators".into(), Fact::List(list));
    }

    /// A `reexport` symbol with a `reexports` reference to `candidates`; `export_as` is the
    /// name other files import it by (none for `export *`).
    fn reexport(
        &mut self,
        name: &str,
        export_as: Option<String>,
        candidates: Vec<ExportKey>,
        span: Span,
    ) {
        let mut facts = Self::base_facts(true, name == "default", false);
        facts.insert("reexport".into(), Fact::Bool(true));
        let id = self.add_symbol(vec![Self::seg("reexport", name)], "reexport", span, facts);
        if let Some(n) = export_as {
            self.export(NS_EXPORT, export_name(self.path.as_str(), &n), id);
        }
        if !candidates.is_empty() {
            self.push_ref(
                id,
                "reexports",
                Some(span),
                candidates,
                Confidence::Extracted,
            );
        }
    }

    /// Resolve `export { a as b }` and `export default a` once every declaration is known.
    pub fn finish_exports(&mut self) {
        for (local, exported, span) in std::mem::take(&mut self.export_locals) {
            if let Some(b) = self.imports.get(&local).cloned() {
                let cands = match b {
                    Binding::Named { modules, name } => export_candidates(&modules, &name),
                    Binding::Namespace { modules } => modules,
                };
                self.reexport(&exported, Some(exported.clone()), cands, span);
            } else if let Some(&id) = self.declared.get(&local) {
                self.export(NS_EXPORT, export_name(self.path.as_str(), &exported), id);
            }
        }
        let path = self.path.as_str().to_string();
        let artifact = self.artifact_id;
        self.export(NS_FILE, path, artifact);
    }
}

fn body_start(a: &ArrowFunctionExpression<'_>) -> u32 {
    a.body.span().start
}

fn inner_start(e: &Expression<'_>) -> u32 {
    e.span().start
}
