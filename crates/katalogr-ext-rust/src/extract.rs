//! Source-file extraction (FEAT-005 sections 4 and 8): one pass over the syntax tree builds a
//! [`Fragment`]. A syntax error never fails the file: the tree keeps what parsed.

use crate::context::{self, CrateInfo, TargetKind, WorkspaceContext};
use crate::lines::LineIndex;
use crate::ownership::owner_of;
use crate::roots::{self, RootCtx};
use katalogr_model::annotate;
use katalogr_model::insight::{FACT_PACKAGE, FACT_RATIONALE, FACT_ROOT};
use katalogr_model::{
    ArtifactName, ArtifactNode, Confidence, Export, ExportKey, ExtractError, ExtractInput, Fact,
    Fragment, Id, Link, LinkKind, Name, OutRef, Provenance, Segment, Span, SymbolName, SymbolNode,
};
use ra_ap_syntax::{
    AstNode, Edition, SourceFile, SyntaxElement, SyntaxKind, SyntaxNode, TextRange,
    ast::{self, HasAttrs, HasModuleItem},
};
use std::collections::{BTreeMap, HashMap};

pub const NS_ITEM: &str = "rs-item";
pub const NS_FILE: &str = "rs-file";
pub const NS_METHOD: &str = "rs-method";
pub const NS_MACRO: &str = "rs-macro";
pub use crate::manifest::NS_PACKAGE;

/// The name a path resolves to: an item of a crate in this repository, or a package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// Crate directory as it appears in keys, and the path below the crate root.
    Crate(String, Vec<String>),
    Package(String),
}

impl Target {
    pub(crate) fn key(&self) -> ExportKey {
        match self {
            Target::Crate(dir, segs) => item_key(dir, segs),
            Target::Package(p) => ExportKey {
                namespace: NS_PACKAGE.to_string(),
                key: p.clone(),
            },
        }
    }

    pub(crate) fn extended(&self, rest: &[String]) -> Target {
        match self {
            Target::Crate(d, s) => {
                Target::Crate(d.clone(), s.iter().chain(rest).cloned().collect())
            }
            p @ Target::Package(_) => p.clone(),
        }
    }
}

/// `("rs-item", "<crate dir>::<module path>::<name>")`.
pub(crate) fn item_key(dir: &str, segs: &[String]) -> ExportKey {
    let mut key = dir.to_string();
    for s in segs {
        key.push_str("::");
        key.push_str(s);
    }
    ExportKey {
        namespace: NS_ITEM.to_string(),
        key,
    }
}

/// The names bound in one module (by its own `use` items) and its glob imports.
pub(crate) struct Scope {
    pub path: Vec<String>,
    /// Names of the inline modules below the file's own directory, for `mod x;` files.
    pub fs: Vec<String>,
    pub bindings: BTreeMap<String, Target>,
    pub globs: Vec<Target>,
}

pub(crate) struct Builder<'a> {
    pub artifact: ArtifactName,
    pub provenance: Provenance,
    pub lines: LineIndex<'a>,
    pub krate: Option<&'a CrateInfo>,
    /// The owning crate's directory, as written in keys.
    pub own: Option<String>,
    /// Directory in which this file's `mod x;` children are looked up.
    pub child_dir: String,
    pub symbols: Vec<SymbolNode>,
    pub links: Vec<Link>,
    pub refs: Vec<OutRef>,
    pub exports: Vec<Export>,
    ordinals: BTreeMap<Vec<Segment>, u32>,
    pub segs: Vec<Segment>,
    pub ids: Vec<Id>,
    pub scopes: Vec<Scope>,
    /// Last path segment of the current impl's self type, or the trait name inside a trait.
    pub self_ident: Option<String>,
    /// Kind of the target that owns the file.
    pub target_kind: Option<TargetKind>,
    /// The file is the root file of its target (so its top level is the target's root module).
    pub target_root: bool,
    /// Depth of enclosing `#[cfg(test)]` modules (the file counts when it is `#![cfg(test)]`).
    pub test_depth: u32,
    /// Inside the items of `impl Trait for Type`.
    pub trait_impl: bool,
    /// Node range of each symbol, to attribute comments.
    pub ranges: HashMap<TextRange, Id>,
}

pub fn extract(input: &ExtractInput) -> Result<Fragment, ExtractError> {
    extract_with_errors(input).map(|(fragment, _)| fragment)
}

/// A single-file target under `src/bin` or `examples`: its module path is the file stem.
fn conventional_root(owner: &crate::ownership::Owner<'_>, path: &str) -> bool {
    let (Some(t), [stem]) = (&owner.target, owner.module.as_slice()) else {
        return false;
    };
    let dir = context::parent_dir(path);
    let file_ok = path.rsplit('/').next() == Some(&format!("{}.rs", t.name)) && *stem == t.name;
    file_ok && (dir.ends_with("src/bin") || dir.ends_with("examples") || dir.ends_with("benches"))
}

/// The fragment plus one [`ExtractError::Parse`] (with span) per syntax error in the file.
pub fn extract_with_errors(
    input: &ExtractInput,
) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
    let text = std::str::from_utf8(input.content).map_err(|e| ExtractError::Parse {
        span: None,
        message: format!("not utf-8: {e}"),
    })?;
    let ctx = WorkspaceContext::from_bytes(input.context)?;
    let owner = owner_of(&ctx, input.path);
    let artifact = ArtifactName {
        repo: input.repo.to_string(),
        path: input.path.clone(),
    };
    let provenance = Provenance {
        repo: input.repo.to_string(),
        path: input.path.clone(),
        revision: input.revision.to_string(),
    };

    let path = input.path.as_str();
    let file_dir = context::parent_dir(path);
    let stem = path
        .rsplit('/')
        .next()
        .and_then(|f| f.strip_suffix(".rs"))
        .unwrap_or("");
    let child_dir = if owner.module.is_empty() || stem == "mod" {
        file_dir.to_string()
    } else {
        context::join_norm(file_dir, stem).unwrap_or_default()
    };

    let mut b = Builder {
        artifact: artifact.clone(),
        provenance: provenance.clone(),
        lines: LineIndex::new(text),
        krate: owner.krate,
        own: owner.krate.map(|c| context::key_dir(&c.dir)),
        child_dir,
        symbols: Vec::new(),
        links: Vec::new(),
        refs: Vec::new(),
        exports: Vec::new(),
        ordinals: BTreeMap::new(),
        segs: Vec::new(),
        ids: Vec::new(),
        scopes: vec![Scope {
            path: owner.module.clone(),
            fs: Vec::new(),
            bindings: BTreeMap::new(),
            globs: Vec::new(),
        }],
        self_ident: None,
        target_kind: owner.target.as_ref().map(|t| t.kind),
        target_root: owner.module.is_empty() || conventional_root(&owner, path),
        test_depth: 0,
        trait_impl: false,
        ranges: HashMap::new(),
    };

    let mut facts: BTreeMap<String, Fact> = BTreeMap::new();
    if let Some(t) = &owner.target {
        facts.insert(
            "target".to_string(),
            Fact::Str(format!("{}:{}", t.kind.as_str(), t.name)),
        );
    }
    if owner.orphan {
        facts.insert("orphan".to_string(), Fact::Bool(true));
    }
    if let Some(k) = owner.krate {
        facts.insert(FACT_PACKAGE.to_string(), Fact::Str(k.name.clone()));
    }

    // Syntax errors do not stop the walk; `ra_ap_syntax` keeps the tree it could build.
    let parse = SourceFile::parse(text, Edition::Edition2021);
    let tree = parse.tree();
    let inner = attr_texts(tree.attrs());
    if inner
        .iter()
        .any(|a| matches!(a, Fact::Str(t) if roots::is_cfg_test(t)))
    {
        b.test_depth = 1;
    }
    if !inner.is_empty() {
        facts.insert("attrs".to_string(), Fact::List(inner));
    }
    b.export(
        ExportKey {
            namespace: NS_FILE.to_string(),
            key: path.to_string(),
        },
        artifact.id(),
    );
    b.items(tree.items().collect());
    // Rationale markers go to the innermost symbol that holds the comment, else to the file.
    for (owner_id, markers) in crate::rationale::collect(tree.syntax(), text, &b.lines, &b.ranges) {
        let fact = annotate::rationale_fact(&markers);
        match owner_id {
            None => {
                facts.insert(FACT_RATIONALE.to_string(), fact);
            }
            Some(id) => {
                if let Some(sym) = b.symbols.iter_mut().find(|s| s.name.id() == id) {
                    sym.facts.insert(FACT_RATIONALE.to_string(), fact);
                }
            }
        }
    }
    let errors = parse
        .errors()
        .iter()
        .map(|e| {
            let r = e.range();
            ExtractError::Parse {
                span: Some(b.lines.span(r.start().into(), r.end().into())),
                message: e.to_string(),
            }
        })
        .collect();

    Ok((
        Fragment {
            provenance,
            extension: "rust".to_string(),
            artifact: ArtifactNode {
                name: artifact,
                facts,
            },
            symbols: b.symbols,
            links: b.links,
            refs: b.refs,
            exports: b.exports,
            bindings: Vec::new(),
        },
        errors,
    ))
}

impl Builder<'_> {
    pub(crate) fn scope(&self) -> &Scope {
        self.scopes
            .last()
            .expect("a file always has its module scope")
    }

    pub(crate) fn module_path(&self) -> Vec<String> {
        self.scope().path.clone()
    }

    pub(crate) fn export(&mut self, key: ExportKey, target: Id) {
        self.exports.push(Export { key, target });
    }

    /// Export `("rs-item", ...)` for a name below the current module.
    pub(crate) fn export_item(&mut self, below_module: &[&str], target: Id) {
        let Some(own) = self.own.clone() else {
            return;
        };
        let mut segs = self.module_path();
        segs.extend(below_module.iter().map(|s| s.to_string()));
        self.export(item_key(&own, &segs), target);
    }

    pub(crate) fn span_of(&self, node: &SyntaxNode) -> Span {
        let (s, e) = trimmed(node);
        self.lines.span(s, e)
    }

    pub(crate) fn add_ref(
        &mut self,
        from: Id,
        kind: &str,
        span: Span,
        candidates: Vec<ExportKey>,
        confidence: Confidence,
    ) {
        let mut seen = Vec::new();
        for c in candidates {
            if !seen.contains(&c) {
                seen.push(c);
            }
        }
        if seen.is_empty() {
            return;
        }
        self.refs.push(OutRef {
            from,
            kind: LinkKind(kind.to_string()),
            span: Some(span),
            candidates: seen,
            confidence,
            origin: None,
        });
    }

    /// Record a symbol below the current nesting; links it from its parent with `contains`.
    pub(crate) fn add_symbol(
        &mut self,
        kind: &str,
        name: &str,
        node: &SyntaxNode,
        mut facts: BTreeMap<String, Fact>,
    ) -> Id {
        self.root_fact(kind, name, node, &mut facts);
        if let Some(doc) = doc_first_paragraph(node) {
            facts.insert("doc".to_string(), Fact::Str(doc));
        }
        let segment = Segment {
            kind: kind.to_string(),
            name: name.to_string(),
        };
        let mut segments = self.segs.clone();
        segments.push(segment);
        let n = self.ordinals.entry(segments.clone()).or_insert(0);
        *n += 1;
        let sym = SymbolName {
            artifact: self.artifact.clone(),
            segments,
            ordinal: *n,
        };
        let id = sym.id();
        let span = self.span_of(node);
        if let Some(&parent) = self.ids.last() {
            self.links.push(Link {
                from: parent,
                to: id,
                kind: LinkKind("contains".to_string()),
                confidence: Confidence::Extracted,
                span: Some(span),
                provenance: self.provenance.clone(),
            });
        }
        self.symbols.push(SymbolNode {
            name: sym,
            kind: kind.to_string(),
            span,
            facts,
        });
        self.ranges.insert(node.text_range(), id);
        self.annotations(id, node);
        id
    }

    /// Set the `root` fact when the symbol is an entry point (see [`roots::root_reason`]).
    fn root_fact(
        &self,
        kind: &str,
        name: &str,
        node: &SyntaxNode,
        facts: &mut BTreeMap<String, Fact>,
    ) {
        let attrs: Vec<String> = match facts.get("attrs") {
            Some(Fact::List(l)) => l
                .iter()
                .filter_map(|f| match f {
                    Fact::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let vis = match facts.get("vis") {
            Some(Fact::Str(v)) => Some(v.as_str()),
            _ => None,
        };
        let extern_abi = ast::Fn::cast(node.clone()).is_some_and(|f| f.abi().is_some());
        let reason = roots::root_reason(&RootCtx {
            kind,
            name,
            attrs: &attrs,
            vis,
            target: self.target_kind,
            at_target_root: self.target_root && self.segs.is_empty(),
            in_test_code: self.test_depth > 0,
            in_trait_impl: self.trait_impl,
            extern_abi,
        });
        if let Some(r) = reason {
            facts.insert(FACT_ROOT.to_string(), Fact::Str(r.to_string()));
        }
    }

    /// FEAT-019 T6: item annotations (`FEAT-001 R-1`) in the comments that lead the item (doc
    /// comments included) become `annotated-by` references from the symbol.
    fn annotations(&mut self, id: Id, node: &SyntaxNode) {
        for el in node.children_with_tokens() {
            match el.kind() {
                SyntaxKind::COMMENT | SyntaxKind::DOC_COMMENT => {
                    let range = el.text_range();
                    let span = self.lines.span(range.start().into(), range.end().into());
                    let text = el.to_string();
                    for r in annotate::scan(&text) {
                        let keys = annotate::keys(&r);
                        let dup = self.refs.iter().any(|x| {
                            x.from == id && x.kind.0 == "annotated-by" && x.candidates == keys
                        });
                        if !dup {
                            self.add_ref(id, "annotated-by", span, keys, Confidence::Extracted);
                        }
                    }
                }
                SyntaxKind::WHITESPACE | SyntaxKind::ATTR => {}
                _ => break,
            }
        }
    }

    pub(crate) fn enter(&mut self, kind: &str, name: &str, id: Id) {
        self.segs.push(Segment {
            kind: kind.to_string(),
            name: name.to_string(),
        });
        self.ids.push(id);
    }

    pub(crate) fn leave(&mut self) {
        self.segs.pop();
        self.ids.pop();
    }

    /// Facts every item carries: `vis`, `attrs` (when any) and `signature`.
    pub(crate) fn facts(
        &self,
        vis: Option<ast::Visibility>,
        attrs: ast::AstChildren<ast::Attr>,
        node: &SyntaxNode,
    ) -> BTreeMap<String, Fact> {
        let mut f = BTreeMap::new();
        let vis = vis.map_or_else(
            || "private".to_string(),
            |v| {
                v.syntax()
                    .text()
                    .to_string()
                    .split_whitespace()
                    .collect::<String>()
            },
        );
        f.insert("vis".to_string(), Fact::Str(vis));
        let attrs = attr_texts(attrs);
        if !attrs.is_empty() {
            f.insert("attrs".to_string(), Fact::List(attrs));
        }
        f.insert("signature".to_string(), Fact::Str(signature(node)));
        f
    }
}

/// The first paragraph of the outer doc comments of an item (`///` lines and `/** */` blocks),
/// whitespace normalised to one line. `None` when the item has no doc comment or it is empty.
pub(crate) fn doc_first_paragraph(node: &SyntaxNode) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for e in node.children_with_tokens() {
        match e.kind() {
            SyntaxKind::COMMENT | SyntaxKind::DOC_COMMENT => {
                let text = e.to_string();
                if let Some(rest) = text.strip_prefix("///") {
                    if !rest.starts_with('/') {
                        lines.push(rest.trim().to_string());
                    }
                } else if let Some(rest) = text.strip_prefix("/**")
                    && !rest.starts_with(['*', '/'])
                {
                    let body = rest.strip_suffix("*/").unwrap_or(rest);
                    for l in body.lines() {
                        let l = l.trim();
                        lines.push(l.strip_prefix('*').unwrap_or(l).trim().to_string());
                    }
                }
            }
            SyntaxKind::WHITESPACE | SyntaxKind::ATTR => {}
            // The doc comments lead the item: stop at the first real token.
            _ => break,
        }
    }
    let paragraph: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .skip_while(|l| l.is_empty())
        .take_while(|l| !l.is_empty())
        .collect();
    let doc = paragraph.join(" ");
    (!doc.is_empty()).then_some(doc)
}

/// Attribute texts without the `#[` `]` wrapper, whitespace normalised.
pub(crate) fn attr_texts(attrs: ast::AstChildren<ast::Attr>) -> Vec<Fact> {
    attrs
        .filter_map(|a| a.meta().map(|m| normalise(&m.syntax().text().to_string())))
        .map(Fact::Str)
        .collect()
}

pub(crate) fn normalise(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_trivia(e: &SyntaxElement) -> bool {
    matches!(e.kind(), SyntaxKind::WHITESPACE | SyntaxKind::COMMENT)
}

/// The byte range of a node without leading and trailing whitespace and comments.
pub(crate) fn trimmed(node: &SyntaxNode) -> (usize, usize) {
    let range = node.text_range();
    let mut start: usize = range.start().into();
    let mut end: usize = range.end().into();
    let elems: Vec<SyntaxElement> = node.children_with_tokens().collect();
    if let Some(first) = elems.iter().find(|e| !is_trivia(e)) {
        start = first.text_range().start().into();
    }
    if let Some(last) = elems.iter().rev().find(|e| !is_trivia(e)) {
        end = last.text_range().end().into();
    }
    (start, end)
}

/// One-line header of an item: everything before its body, `=` or `;`, without attributes.
pub(crate) fn signature(node: &SyntaxNode) -> String {
    let mut out = String::new();
    for e in node.children_with_tokens() {
        if matches!(
            e.kind(),
            SyntaxKind::COMMENT | SyntaxKind::DOC_COMMENT | SyntaxKind::ATTR
        ) {
            continue;
        }
        if matches!(
            e.kind(),
            SyntaxKind::BLOCK_EXPR
                | SyntaxKind::RECORD_FIELD_LIST
                | SyntaxKind::VARIANT_LIST
                | SyntaxKind::ASSOC_ITEM_LIST
                | SyntaxKind::ITEM_LIST
                | SyntaxKind::TOKEN_TREE
                | SyntaxKind::SEMICOLON
                | SyntaxKind::L_CURLY
                | SyntaxKind::EQ
        ) {
            break;
        }
        out.push_str(&e.to_string());
    }
    normalise(&out)
}
