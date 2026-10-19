//! JSON Schema: named schemas and their properties as symbols (FEAT-009 section 6.2). Public
//! so that API-document extensions can reuse it on the component schemas of their documents.

use crate::build::Builder;
use crate::paths::{file_key, first_paragraph, has_scheme, normalise, stem};
use crate::qname::{clark, json_primitive};
use katalogr_ext_data::tree::{Entry, Node, Scalar, Value};
use katalogr_model::{Confidence, ContractRole, ExportKey, ExtractError, Fact, RelPath, Span};
use std::collections::{BTreeMap, BTreeSet};

const TYPE: &str = "schema-type";
const TYPE_AT: &str = "schema-type-at";
const MEMBER: &str = "schema-member";

/// Walk the named schemas of `doc` into `b`; returns the recoverable errors.
pub fn extract_schemas(
    b: &mut Builder,
    doc: &Node,
    repo: &str,
    path: &RelPath,
) -> Vec<ExtractError> {
    let mut w = W {
        b,
        repo,
        path,
        ns: String::new(),
        has_id: false,
        root_name: None,
        external: BTreeSet::new(),
    };
    w.run(doc);
    let ext = std::mem::take(&mut w.external);
    if !ext.is_empty() {
        w.b.artifact_fact(
            "external_refs",
            Fact::List(ext.into_iter().map(Fact::Str).collect()),
        );
    }
    Vec::new()
}

struct W<'a> {
    b: &'a mut Builder,
    repo: &'a str,
    path: &'a RelPath,
    ns: String,
    has_id: bool,
    root_name: Option<String>,
    external: BTreeSet<String>,
}

fn s(v: &str) -> Fact {
    Fact::Str(v.to_string())
}

/// The key's span to the end of its value.
fn entry_span(e: &Entry) -> Span {
    let (k, v) = (e.key_span, e.value.span);
    if (v.end_line, v.end_col) >= (k.end_line, k.end_col) {
        Span {
            start_line: k.start_line,
            start_col: k.start_col,
            end_line: v.end_line,
            end_col: v.end_col,
        }
    } else {
        k
    }
}

fn text<'n>(n: &'n Node, key: &str) -> Option<&'n str> {
    match n.get(key)?.value {
        Value::Scalar(Scalar::Str(_)) => n.get(key)?.as_text(),
        _ => None,
    }
}

fn doc_of(n: &Node) -> Option<String> {
    first_paragraph(text(n, "description")?)
}

/// What a `$ref` points at.
enum Target {
    /// A schema of this file.
    Local(String),
    /// A schema of another file of the repository (`None`: the file's root schema).
    File { path: String, name: Option<String> },
    /// A schema named by an absolute URI.
    Absolute { uri: String, name: Option<String> },
    /// A pointer into a schema, or a path that leaves the repository.
    Other,
}

/// The name in `/$defs/X`, `/definitions/X` or `/components/schemas/X`.
fn pointer_name(frag: &str) -> Option<String> {
    let rest = frag.strip_prefix('/')?;
    let parts: Vec<&str> = rest.split('/').collect();
    let name = match parts.as_slice() {
        ["$defs" | "definitions", n] | ["components", "schemas", n] => n,
        _ => return None,
    };
    Some(name.replace("~1", "/").replace("~0", "~"))
}

/// What a property's type resolves to.
#[derive(Default)]
struct Shape<'n> {
    ty: String,
    many: bool,
    /// The schema whose properties become nested fields.
    inline: Option<&'n Node>,
    /// A reference: kind, candidates, and the imported file.
    refs: Vec<(&'static str, Vec<ExportKey>, Option<ExportKey>)>,
    ref_raw: Option<String>,
    variants: Vec<String>,
}

impl<'a> W<'a> {
    fn run(&mut self, doc: &Node) {
        if !doc.is_map() {
            return;
        }
        let api = ["openapi", "swagger", "asyncapi"]
            .iter()
            .find(|k| doc.get(k).is_some())
            .copied();
        let dialect = match api {
            Some("openapi") => "openapi-3",
            Some("swagger") => "swagger-2",
            Some(_) => "asyncapi",
            None => {
                let sch = text(doc, "$schema").unwrap_or("");
                ["draft-04", "draft-06", "draft-07", "2019-09", "2020-12"]
                    .into_iter()
                    .find(|d| sch.contains(d))
                    .unwrap_or("unknown")
            }
        };
        self.b.artifact_fact("dialect", s(dialect));
        let id =
            text(doc, "$id").or_else(|| (dialect == "draft-04").then(|| text(doc, "id")).flatten());
        if let Some(id) = id {
            let id = id.split('#').next().unwrap_or("");
            if !id.is_empty() {
                self.ns = id.to_string();
                self.has_id = true;
            }
        }
        self.b
            .artifact_fact("namespace", Fact::Str(self.ns.clone()));

        let mut named: Vec<(String, &Node, bool)> = Vec::new();
        if api.is_none() && (doc.get("properties").is_some() || doc.get("type").is_some()) {
            let title = text(doc, "title")
                .map(|t| {
                    t.chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
                        .collect::<String>()
                })
                .filter(|t| !t.is_empty());
            let name = title.unwrap_or_else(|| {
                let st = stem(self.path);
                st.strip_suffix(".schema").unwrap_or(st).to_string()
            });
            self.root_name = Some(name.clone());
            named.push((name, doc, true));
        }
        for key in ["$defs", "definitions"] {
            if let Some(m) = doc.get(key) {
                for e in m.entries() {
                    named.push((e.key.clone(), &e.value, false));
                }
            }
        }
        if let Some(m) = doc.get("components").and_then(|c| c.get("schemas")) {
            for e in m.entries() {
                named.push((e.key.clone(), &e.value, false));
            }
        }
        for (name, node, is_root) in named {
            self.named_type(&name, node, is_root);
        }
    }

    fn form_of(n: &Node) -> &'static str {
        if n.get("enum").is_some() {
            return "enum";
        }
        match n.get("type").and_then(Node::as_text) {
            Some("object") => "object",
            Some("array") => "array",
            Some("string" | "integer" | "number" | "boolean" | "null") => "scalar",
            _ => {
                if n.get("properties").is_some()
                    || n.get("allOf").is_some()
                    || n.get("$ref").is_some()
                    || n.get("oneOf").is_some()
                    || n.get("anyOf").is_some()
                {
                    "object"
                } else {
                    "scalar"
                }
            }
        }
    }

    fn enum_fact(n: &Node) -> Option<Fact> {
        let items = n.get("enum")?.items();
        let list: Vec<Fact> = items.iter().filter_map(|i| i.as_text().map(s)).collect();
        (!list.is_empty()).then_some(Fact::List(list))
    }

    fn named_type(&mut self, name: &str, n: &Node, is_root: bool) {
        let key = clark(&self.ns, name);
        let mut f: BTreeMap<String, Fact> = BTreeMap::new();
        f.insert("qname".to_string(), Fact::Str(key.clone()));
        f.insert("namespace".to_string(), Fact::Str(self.ns.clone()));
        f.insert("form".to_string(), s(Self::form_of(n)));
        if let Some(d) = doc_of(n) {
            f.insert("doc".to_string(), Fact::Str(d));
        }
        if let Some(e) = Self::enum_fact(n) {
            f.insert("enum".to_string(), e);
        }
        let idx = self.b.symbol(None, "type", name, n.span, f);
        self.b.export(idx, TYPE, key.clone());
        self.b
            .export(idx, TYPE_AT, format!("{}:{}#{name}", self.repo, self.path));
        if is_root {
            // The file's root schema is what `$ref` to the whole file, or to the bare `$id`,
            // names.
            self.b
                .export(idx, TYPE_AT, format!("{}:{}#", self.repo, self.path));
            if self.has_id {
                self.b.export(idx, TYPE, clark(&self.ns, ""));
            }
        }
        self.b.bind(
            idx,
            "schema_type",
            key.clone(),
            ContractRole::Declares,
            n.span,
            if self.has_id {
                Confidence::Extracted
            } else {
                Confidence::Inferred
            },
        );
        self.walk_object(idx, &key, n, &BTreeSet::new());
    }

    /// The names a `required` array lists.
    fn required_of(n: &Node) -> BTreeSet<String> {
        n.get("required")
            .map(|r| {
                r.items()
                    .iter()
                    .filter_map(|i| i.as_text().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn walk_object(
        &mut self,
        owner: usize,
        owner_key: &str,
        schema: &Node,
        outer_required: &BTreeSet<String>,
    ) {
        if !schema.is_map() {
            return;
        }
        let mut required = Self::required_of(schema);
        required.extend(outer_required.iter().cloned());
        if let Some(props) = schema.get("properties") {
            for e in props.entries() {
                self.field(owner, owner_key, e, required.contains(&e.key));
            }
        }
        // A schema that is just a reference is derived from its target.
        if let Some(r) = text(schema, "$ref") {
            let span = schema.get("$ref").map_or(schema.span, |n| n.span);
            self.extends(owner, r, span);
        }
        if let Some(all) = schema.get("allOf") {
            for m in all.items() {
                match text(m, "$ref") {
                    Some(r) => self.extends(owner, r, m.span),
                    None => self.walk_object(owner, owner_key, m, &required),
                }
            }
        }
        let mut variants = Vec::new();
        for key in ["oneOf", "anyOf"] {
            if let Some(list) = schema.get(key) {
                for m in list.items() {
                    if let Some(r) = text(m, "$ref")
                        && let Some((k, refs, import)) = self.resolve(r)
                    {
                        self.emit(owner, "uses-type", refs, import, m.span);
                        variants.push(k);
                    }
                }
            }
        }
        if !variants.is_empty() {
            self.b.fact(
                owner,
                "variants",
                Fact::List(variants.into_iter().map(Fact::Str).collect()),
            );
        }
        if let Some(items) = schema.get("items")
            && let Some(r) = text(items, "$ref")
            && let Some((_, refs, import)) = self.resolve(r)
        {
            self.emit(owner, "uses-type", refs, import, items.span);
        }
    }

    /// `extends` from `owner` to the schema `raw` names, and its `base` fact.
    fn extends(&mut self, owner: usize, raw: &str, span: Span) {
        if let Some((key, refs, import)) = self.resolve(raw) {
            if self.b.get_fact(owner, "base").is_none() {
                self.b.fact(owner, "base", Fact::Str(key));
            }
            self.emit(owner, "extends", refs, import, span);
        }
    }

    /// A reference, and the `imports` of the file it names.
    fn emit(
        &mut self,
        from: usize,
        kind: &str,
        candidates: Vec<ExportKey>,
        import: Option<ExportKey>,
        span: Span,
    ) {
        self.b
            .reference(from, kind, candidates, span, Confidence::Extracted);
        if let Some(i) = import {
            self.b
                .reference(from, "imports", vec![i], span, Confidence::Extracted);
        }
    }

    fn classify(&mut self, raw: &str) -> Target {
        if raw == "#" {
            return self.root_name.clone().map_or(Target::Other, Target::Local);
        }
        if let Some(frag) = raw.strip_prefix('#') {
            return pointer_name(frag).map_or(Target::Other, Target::Local);
        }
        let (base, frag) = raw.split_once('#').unwrap_or((raw, ""));
        let name = pointer_name(frag);
        if has_scheme(base) {
            self.external.insert(base.to_string());
            return Target::Absolute {
                uri: base.to_string(),
                name,
            };
        }
        match normalise(self.path, base) {
            Some(path) => Target::File { path, name },
            None => {
                self.external.insert(base.to_string());
                Target::Other
            }
        }
    }

    /// The key, export candidates and imported file of a `$ref`; None when it names no schema.
    fn resolve(&mut self, raw: &str) -> Option<(String, Vec<ExportKey>, Option<ExportKey>)> {
        let cand = |ns: &str, key: String| ExportKey {
            namespace: ns.to_string(),
            key,
        };
        match self.classify(raw) {
            Target::Local(name) => {
                let key = clark(&self.ns, &name);
                Some((
                    key.clone(),
                    vec![
                        cand(TYPE_AT, format!("{}:{}#{name}", self.repo, self.path)),
                        cand(TYPE, key),
                    ],
                    None,
                ))
            }
            Target::File { path, name } => {
                let import = Some(file_key(self.repo, &path));
                match name {
                    Some(n) => {
                        let key = clark("", &n);
                        Some((
                            key.clone(),
                            vec![
                                cand(TYPE_AT, format!("{}:{path}#{n}", self.repo)),
                                cand(TYPE, key),
                            ],
                            import,
                        ))
                    }
                    None => {
                        let file = path.rsplit('/').next().unwrap_or("");
                        let st = file.rsplit_once('.').map_or(file, |(s, _)| s);
                        let n = st.strip_suffix(".schema").unwrap_or(st);
                        Some((
                            clark("", n),
                            vec![cand(TYPE_AT, format!("{}:{path}#", self.repo))],
                            import,
                        ))
                    }
                }
            }
            Target::Absolute { uri, name } => {
                let key = clark(&uri, name.as_deref().unwrap_or(""));
                Some((key.clone(), vec![cand(TYPE, key)], None))
            }
            Target::Other => None,
        }
    }

    // ---- properties ----

    fn shape<'n>(&mut self, prop: &'n Node) -> Shape<'n> {
        let mut sh = Shape {
            ty: "any".to_string(),
            ..Shape::default()
        };
        if !prop.is_map() {
            return sh;
        }
        let ref_text = text(prop, "$ref").or_else(|| {
            prop.get("allOf")?
                .items()
                .iter()
                .find_map(|m| text(m, "$ref"))
        });
        if let Some(r) = ref_text {
            match self.resolve(r) {
                Some((key, refs, import)) => {
                    sh.ty = key;
                    sh.refs.push(("uses-type", refs, import));
                }
                None => sh.ref_raw = Some(r.to_string()),
            }
            return sh;
        }
        let ty = match prop.get("type").map(|t| &t.value) {
            Some(Value::Scalar(_)) => prop.get("type").and_then(Node::as_text).map(str::to_string),
            Some(Value::Seq(list)) => {
                let named: Vec<&str> = list
                    .iter()
                    .filter_map(Node::as_text)
                    .filter(|t| *t != "null")
                    .collect();
                (named.len() == 1).then(|| named[0].to_string())
            }
            _ => None,
        };
        match ty.as_deref() {
            Some("array") => {
                sh.many = true;
                match prop.get("items") {
                    Some(items) if items.is_map() => {
                        let inner = self.shape(items);
                        sh.ty = inner.ty;
                        sh.inline = inner.inline;
                        sh.refs = inner.refs;
                        sh.ref_raw = inner.ref_raw;
                        sh.variants = inner.variants;
                    }
                    _ => {}
                }
            }
            Some("object") => {
                sh.ty = "object".to_string();
                if prop.get("properties").is_some() || prop.get("allOf").is_some() {
                    sh.inline = Some(prop);
                }
            }
            Some(t) => sh.ty = json_primitive(t, text(prop, "format")).to_string(),
            None => {
                if prop.get("properties").is_some() {
                    sh.ty = "object".to_string();
                    sh.inline = Some(prop);
                } else if prop.get("type").is_none() {
                    for key in ["oneOf", "anyOf"] {
                        if let Some(list) = prop.get(key) {
                            for m in list.items() {
                                if let Some(r) = text(m, "$ref")
                                    && let Some((k, refs, import)) = self.resolve(r)
                                {
                                    sh.refs.push(("uses-type", refs, import));
                                    sh.variants.push(k);
                                }
                            }
                        }
                    }
                }
            }
        }
        sh
    }

    fn field(&mut self, owner: usize, owner_key: &str, e: &Entry, required: bool) {
        let prop = &e.value;
        let sh = self.shape(prop);
        let mut f: BTreeMap<String, Fact> = BTreeMap::new();
        f.insert("field_type".to_string(), Fact::Str(sh.ty.clone()));
        f.insert("required".to_string(), Fact::Bool(required));
        f.insert("many".to_string(), Fact::Bool(sh.many));
        if let Some(fm) = text(prop, "format") {
            f.insert("format".to_string(), s(fm));
        }
        if let Some(en) = Self::enum_fact(prop) {
            f.insert("enum".to_string(), en);
        }
        if let Some(d) = prop.get("default").and_then(|d| match d.value {
            Value::Scalar(_) => d.as_text(),
            _ => None,
        }) {
            f.insert("default".to_string(), s(d));
        }
        if let Some(d) = doc_of(prop) {
            f.insert("doc".to_string(), Fact::Str(d));
        }
        if let Some(r) = &sh.ref_raw {
            f.insert("ref".to_string(), Fact::Str(r.clone()));
        }
        if !sh.variants.is_empty() {
            f.insert(
                "variants".to_string(),
                Fact::List(sh.variants.iter().cloned().map(Fact::Str).collect()),
            );
        }
        let span = entry_span(e);
        let idx = self.b.symbol(Some(owner), "field", &e.key, span, f);
        let member = format!("{owner_key}/{}", e.key);
        self.b.export(idx, MEMBER, member.clone());
        for (kind, refs, import) in sh.refs {
            self.emit(idx, kind, refs, import, span);
        }
        if let Some(inline) = sh.inline {
            self.walk_object(idx, &member, inline, &BTreeSet::new());
        }
    }
}
