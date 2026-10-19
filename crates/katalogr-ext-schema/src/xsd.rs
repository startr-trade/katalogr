//! XSD: named types, elements, groups and their members as symbols (FEAT-009 section 6.1).
//! The tree comes from `katalogr-ext-data`; nothing named by `schemaLocation` is fetched.

use crate::build::Builder;
use crate::paths::{file_key, first_paragraph, normalise, stem};
use crate::qname::{clark, xsd_primitive};
use katalogr_ext_data::tree::{Node, Parsed, parse_error};
use katalogr_model::{
    Confidence, ContractRole, ExportKey, ExtractError, ExtractInput, Fact, Fragment, RelPath, Span,
};
use std::collections::BTreeMap;

const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema";
const TYPE: &str = "schema-type";
const TYPE_AT: &str = "schema-type-at";
const ELEMENT: &str = "schema-element";
const GROUP: &str = "schema-group";
const MEMBER: &str = "schema-member";

pub fn extract(input: &ExtractInput, text: &str, parsed: Parsed) -> (Fragment, Vec<ExtractError>) {
    let mut b = Builder::new(input, "xsd", text);
    let mut errors = parsed.errors;
    b.artifact_fact("schema_kind", Fact::Str("xsd".to_string()));
    let root = parsed.docs.first().and_then(|d| d.entries().first());
    match root {
        None => {
            if errors.is_empty() {
                errors.push(parse_error(None, "not an XML Schema"));
            }
        }
        Some(root) => {
            let mut w = Walker {
                b,
                repo: input.repo,
                path: input.path,
                scopes: Vec::new(),
                tns: String::new(),
                errors: Vec::new(),
                external: Vec::new(),
            };
            let ok = w.schema(&root.key, &root.value);
            if !ok {
                errors.push(parse_error(Some(root.value.span), "not an XML Schema"));
            }
            errors.append(&mut w.errors);
            if !w.external.is_empty() {
                w.external.sort();
                w.external.dedup();
                let list = w.external.iter().cloned().map(Fact::Str).collect();
                w.b.artifact_fact("external_refs", Fact::List(list));
            }
            return (w.b.finish(), errors);
        }
    }
    (b.finish(), errors)
}

struct Walker<'a> {
    b: Builder,
    repo: &'a str,
    path: &'a RelPath,
    /// Prefix (empty for the default namespace) to namespace URI, innermost last.
    scopes: Vec<Vec<(String, String)>>,
    tns: String,
    errors: Vec<ExtractError>,
    external: Vec<String>,
}

/// A resolved qualified name.
struct QName {
    ns: String,
    local: String,
}

impl QName {
    fn key(&self) -> String {
        clark(&self.ns, &self.local)
    }

    fn builtin(&self) -> Option<&'static str> {
        (self.ns == XSD_NS)
            .then(|| xsd_primitive(&self.local))
            .flatten()
    }
}

fn attr<'n>(n: &'n Node, name: &str) -> Option<&'n str> {
    n.get(&format!("@{name}")).and_then(Node::as_text)
}

fn s(v: &str) -> Fact {
    Fact::Str(v.to_string())
}

fn facts<const N: usize>(items: [(&str, Fact); N]) -> BTreeMap<String, Fact> {
    items.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

impl Walker<'_> {
    // ---- namespaces ----

    fn push_scope(&mut self, n: &Node) -> bool {
        let decls: Vec<(String, String)> = n
            .entries()
            .iter()
            .filter_map(|e| {
                let prefix = if e.key == "@xmlns" {
                    ""
                } else {
                    e.key.strip_prefix("@xmlns:")?
                };
                Some((prefix.to_string(), e.value.as_text()?.to_string()))
            })
            .collect();
        if decls.is_empty() {
            return false;
        }
        self.scopes.push(decls);
        true
    }

    fn lookup(&self, prefix: &str) -> Option<&str> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|s| s.iter())
            .find(|(p, _)| p == prefix)
            .map(|(_, uri)| uri.as_str())
    }

    /// The local name of an element key when its prefix resolves to the XSD namespace.
    fn xs_local<'k>(&self, key: &'k str) -> Option<&'k str> {
        if key.starts_with(['@', '#']) {
            return None;
        }
        let (prefix, local) = key.split_once(':').unwrap_or(("", key));
        (self.lookup(prefix) == Some(XSD_NS)).then_some(local)
    }

    /// Resolve `p:local` (or an unprefixed name) in the scope of the current element.
    fn resolve(&mut self, text: &str, span: Span) -> Option<QName> {
        let text = text.trim();
        let (prefix, local) = text.split_once(':').unwrap_or(("", text));
        match self.lookup(prefix) {
            Some(uri) => Some(QName {
                ns: uri.to_string(),
                local: local.to_string(),
            }),
            None if prefix.is_empty() => Some(QName {
                ns: String::new(),
                local: local.to_string(),
            }),
            None => {
                self.errors
                    .push(parse_error(Some(span), format!("unknown prefix {prefix}")));
                None
            }
        }
    }

    // ---- the document ----

    /// Walk the root element; false when it is not an XML Schema.
    fn schema(&mut self, key: &str, root: &Node) -> bool {
        let pushed = self.push_scope(root);
        if self.xs_local(key) != Some("schema") {
            if pushed {
                self.scopes.pop();
            }
            return false;
        }
        self.tns = attr(root, "targetNamespace").unwrap_or("").to_string();
        self.b
            .artifact_fact("namespace", Fact::Str(self.tns.clone()));
        let file = stem(self.path).to_string();
        let schema = self.b.symbol(
            None,
            "schema",
            &file,
            root.span,
            facts([("namespace", Fact::Str(self.tns.clone()))]),
        );
        for e in root.entries() {
            let Some(local) = self.xs_local(&e.key).map(str::to_string) else {
                continue;
            };
            let n = &e.value;
            let pushed = self.push_scope(n);
            match local.as_str() {
                "complexType" | "simpleType" => {
                    if let Some(name) = attr(n, "name") {
                        let name = name.to_string();
                        self.named_type(&local, &name, n);
                    }
                }
                "element" => {
                    if let Some(name) = attr(n, "name") {
                        let name = name.to_string();
                        self.global_element(&name, n);
                    }
                }
                "group" | "attributeGroup" => {
                    if let Some(name) = attr(n, "name") {
                        let name = name.to_string();
                        self.group(&local, &name, n);
                    }
                }
                "import" | "include" | "redefine" => {
                    if let Some(loc) = attr(n, "schemaLocation") {
                        let loc = loc.to_string();
                        self.import(schema, &loc, n.span);
                    }
                }
                "annotation" => {
                    if let Some(doc) = self.doc(n) {
                        self.b.fact(schema, "doc", Fact::Str(doc));
                    }
                }
                _ => {}
            }
            if pushed {
                self.scopes.pop();
            }
        }
        if pushed {
            self.scopes.pop();
        }
        true
    }

    fn import(&mut self, schema: usize, location: &str, span: Span) {
        match normalise(self.path, location) {
            Some(p) => self.b.reference(
                schema,
                "imports",
                vec![file_key(self.repo, &p)],
                span,
                Confidence::Extracted,
            ),
            None => self.external.push(location.to_string()),
        }
    }

    /// The first paragraph of the first `documentation` of an `annotation` element.
    fn doc(&self, annotation: &Node) -> Option<String> {
        annotation
            .entries()
            .iter()
            .filter(|e| self.xs_local(&e.key) == Some("documentation"))
            .find_map(|e| {
                let text = e
                    .value
                    .as_text()
                    .or_else(|| e.value.get("#text").and_then(Node::as_text))?;
                first_paragraph(text)
            })
    }

    /// `doc` of a definition from its `annotation` child.
    fn doc_of(&self, n: &Node) -> Option<String> {
        n.entries()
            .iter()
            .filter(|e| self.xs_local(&e.key) == Some("annotation"))
            .find_map(|e| self.doc(&e.value))
    }

    fn children<'n>(&self, n: &'n Node, local: &str) -> Vec<&'n Node> {
        n.entries()
            .iter()
            .filter(|e| self.xs_local(&e.key) == Some(local))
            .map(|e| &e.value)
            .collect()
    }

    // ---- definitions ----

    fn named_type(&mut self, kind: &str, name: &str, n: &Node) {
        let key = clark(&self.tns, name);
        let mut f = facts([
            ("qname", Fact::Str(key.clone())),
            ("namespace", Fact::Str(self.tns.clone())),
            ("form", s("complex")),
        ]);
        if let Some(d) = self.doc_of(n) {
            f.insert("doc".to_string(), Fact::Str(d));
        }
        let idx = self.b.symbol(None, "type", name, n.span, f);
        self.b.export(idx, "schema-type", key.clone());
        self.b
            .export(idx, TYPE_AT, format!("{}:{}#{name}", self.repo, self.path));
        self.b.bind(
            idx,
            "schema_type",
            key.clone(),
            ContractRole::Declares,
            n.span,
            Confidence::Extracted,
        );
        if kind == "simpleType" {
            self.b.fact(idx, "form", s("simple"));
            self.simple_content(idx, n);
        } else {
            self.complex(idx, &key, n, false);
        }
    }

    /// `restriction`, `list` and `union` of a simple type: base, enumeration.
    fn simple_content(&mut self, owner: usize, n: &Node) {
        for r in self.children(n, "restriction") {
            let pushed = self.push_scope(r);
            if let Some(base) = attr(r, "base") {
                let base = base.to_string();
                self.derive(owner, &base, r.span);
            }
            let values: Vec<Fact> = self
                .children(r, "enumeration")
                .into_iter()
                .filter_map(|e| attr(e, "value").map(s))
                .collect();
            if !values.is_empty() {
                self.b.fact(owner, "form", s("enum"));
                self.b.fact(owner, "enum", Fact::List(values));
            }
            if pushed {
                self.scopes.pop();
            }
        }
    }

    /// Record `base` on `owner`, and `extends` when the base is not built in.
    fn derive(&mut self, owner: usize, base: &str, span: Span) {
        let Some(q) = self.resolve(base, span) else {
            return;
        };
        match q.builtin() {
            Some(word) => self.b.fact(owner, "base", s(word)),
            None => {
                let key = q.key();
                self.b.fact(owner, "base", Fact::Str(key.clone()));
                self.b.reference(
                    owner,
                    "extends",
                    vec![ExportKey {
                        namespace: TYPE.to_string(),
                        key,
                    }],
                    span,
                    Confidence::Extracted,
                );
            }
        }
    }

    fn global_element(&mut self, name: &str, n: &Node) {
        let key = clark(&self.tns, name);
        let mut f = facts([("qname", Fact::Str(key.clone()))]);
        if let Some(d) = self.doc_of(n) {
            f.insert("doc".to_string(), Fact::Str(d));
        }
        let idx = self.b.symbol(None, "element", name, n.span, f);
        self.b.export(idx, ELEMENT, key.clone());
        self.b.bind(
            idx,
            "schema_element",
            key.clone(),
            ContractRole::Declares,
            n.span,
            Confidence::Extracted,
        );
        if let Some(t) = attr(n, "type") {
            let t = t.to_string();
            if let Some(q) = self.resolve(&t, n.span) {
                self.b.fact(idx, "type_ref", Fact::Str(q.key()));
                if q.builtin().is_none() {
                    self.b.reference(
                        idx,
                        "uses-type",
                        vec![ExportKey {
                            namespace: TYPE.to_string(),
                            key: q.key(),
                        }],
                        n.span,
                        Confidence::Extracted,
                    );
                }
            }
        }
        for ct in self.children(n, "complexType") {
            let pushed = self.push_scope(ct);
            self.complex(idx, &key, ct, false);
            if pushed {
                self.scopes.pop();
            }
        }
    }

    fn group(&mut self, local: &str, name: &str, n: &Node) {
        let key = clark(&self.tns, name);
        let form = if local == "group" {
            "group"
        } else {
            "attribute-group"
        };
        let mut f = facts([("qname", Fact::Str(key.clone())), ("form", s(form))]);
        if let Some(d) = self.doc_of(n) {
            f.insert("doc".to_string(), Fact::Str(d));
        }
        let idx = self.b.symbol(None, "group", name, n.span, f);
        self.b.export(idx, GROUP, key.clone());
        self.complex(idx, &key, n, false);
    }

    // ---- particles ----

    /// Walk the children of a complex type, group or extension into `owner`.
    fn complex(&mut self, owner: usize, owner_key: &str, n: &Node, in_choice: bool) {
        for e in n.entries() {
            let Some(local) = self.xs_local(&e.key).map(str::to_string) else {
                continue;
            };
            let c = &e.value;
            let pushed = self.push_scope(c);
            match local.as_str() {
                "sequence" | "all" => self.complex(owner, owner_key, c, in_choice),
                "choice" => self.complex(owner, owner_key, c, true),
                "element" => self.element_field(owner, owner_key, c, in_choice),
                "attribute" => self.attribute_field(owner, owner_key, c),
                "group" | "attributeGroup" => {
                    if let Some(r) = attr(c, "ref") {
                        let r = r.to_string();
                        if let Some(q) = self.resolve(&r, c.span) {
                            self.b.reference(
                                owner,
                                "uses-group",
                                vec![ExportKey {
                                    namespace: GROUP.to_string(),
                                    key: q.key(),
                                }],
                                c.span,
                                Confidence::Extracted,
                            );
                        }
                    } else {
                        // An inline group particle: its members belong to the owner.
                        self.complex(owner, owner_key, c, in_choice);
                    }
                }
                "complexContent" | "simpleContent" => {
                    for d in c.entries() {
                        let Some(dl) = self.xs_local(&d.key) else {
                            continue;
                        };
                        if dl != "extension" && dl != "restriction" {
                            continue;
                        }
                        let dn = &d.value;
                        let dp = self.push_scope(dn);
                        if let Some(base) = attr(dn, "base") {
                            let base = base.to_string();
                            self.derive(owner, &base, dn.span);
                        }
                        self.complex(owner, owner_key, dn, in_choice);
                        if dp {
                            self.scopes.pop();
                        }
                    }
                }
                "any" | "anyAttribute" => self.b.fact(owner, "open", Fact::Bool(true)),
                "annotation" => {
                    if self.b.get_fact(owner, "doc").is_none()
                        && let Some(d) = self.doc(c)
                    {
                        self.b.fact(owner, "doc", Fact::Str(d));
                    }
                }
                _ => {}
            }
            if pushed {
                self.scopes.pop();
            }
        }
    }

    fn occurs(c: &Node, in_choice: bool) -> (bool, bool) {
        let required = attr(c, "minOccurs") != Some("0") && !in_choice;
        let many = match attr(c, "maxOccurs") {
            Some("unbounded") => true,
            Some(v) => v.trim().parse::<u64>().is_ok_and(|n| n > 1),
            None => false,
        };
        (required, many)
    }

    fn element_field(&mut self, owner: usize, owner_key: &str, c: &Node, in_choice: bool) {
        let (required, many) = Self::occurs(c, in_choice);
        let mut f = BTreeMap::new();
        f.insert("required".to_string(), Fact::Bool(required));
        f.insert("many".to_string(), Fact::Bool(many));
        f.insert("xml_node".to_string(), s("element"));
        for k in ["minOccurs", "maxOccurs"] {
            if let Some(v) = attr(c, k) {
                let name = if k == "minOccurs" {
                    "min_occurs"
                } else {
                    "max_occurs"
                };
                f.insert(name.to_string(), s(v));
            }
        }
        if let Some(d) = attr(c, "default") {
            f.insert("default".to_string(), s(d));
        }
        if let Some(d) = self.doc_of(c) {
            f.insert("doc".to_string(), Fact::Str(d));
        }
        let (name, ty, uses): (String, String, Option<(&'static str, String)>);
        if let Some(r) = attr(c, "ref") {
            let r = r.to_string();
            let Some(q) = self.resolve(&r, c.span) else {
                return;
            };
            name = q.local.clone();
            ty = q.key();
            uses = Some(("uses-element", q.key()));
        } else if let Some(n) = attr(c, "name") {
            name = n.to_string();
            match attr(c, "type") {
                Some(t) => {
                    let t = t.to_string();
                    let Some(q) = self.resolve(&t, c.span) else {
                        return;
                    };
                    match q.builtin() {
                        Some(w) => {
                            ty = w.to_string();
                            uses = None;
                        }
                        None => {
                            ty = q.key();
                            uses = Some(("uses-type", q.key()));
                        }
                    }
                }
                None => {
                    ty = if self.children(c, "complexType").is_empty() {
                        "any".to_string()
                    } else {
                        "object".to_string()
                    };
                    uses = None;
                }
            }
        } else {
            return;
        }
        f.insert("field_type".to_string(), Fact::Str(ty));
        // An inline simple type: its base and enumeration give the type.
        for st in self.children(c, "simpleType") {
            let pushed = self.push_scope(st);
            for r in self.children(st, "restriction") {
                let rp = self.push_scope(r);
                if let Some(base) = attr(r, "base") {
                    let base = base.to_string();
                    if let Some(q) = self.resolve(&base, r.span) {
                        let t = q.builtin().map_or_else(|| q.key(), str::to_string);
                        f.insert("field_type".to_string(), Fact::Str(t));
                    }
                }
                let values: Vec<Fact> = self
                    .children(r, "enumeration")
                    .into_iter()
                    .filter_map(|e| attr(e, "value").map(s))
                    .collect();
                if !values.is_empty() {
                    f.insert("enum".to_string(), Fact::List(values));
                }
                if rp {
                    self.scopes.pop();
                }
            }
            if pushed {
                self.scopes.pop();
            }
        }
        let idx = self.b.symbol(Some(owner), "field", &name, c.span, f);
        let member = format!("{owner_key}/{name}");
        self.b.export(idx, MEMBER, member.clone());
        if let Some((kind, key)) = uses {
            let ns = if kind == "uses-element" {
                ELEMENT
            } else {
                TYPE
            };
            self.b.reference(
                idx,
                kind,
                vec![ExportKey {
                    namespace: ns.to_string(),
                    key,
                }],
                c.span,
                Confidence::Extracted,
            );
        }
        for ct in self.children(c, "complexType") {
            let pushed = self.push_scope(ct);
            self.b.fact(idx, "field_type", s("object"));
            self.complex(idx, &member, ct, false);
            if pushed {
                self.scopes.pop();
            }
        }
    }

    fn attribute_field(&mut self, owner: usize, owner_key: &str, c: &Node) {
        let mut f = BTreeMap::new();
        f.insert(
            "required".to_string(),
            Fact::Bool(attr(c, "use") == Some("required")),
        );
        f.insert("many".to_string(), Fact::Bool(false));
        f.insert("xml_node".to_string(), s("attribute"));
        if let Some(d) = attr(c, "default") {
            f.insert("default".to_string(), s(d));
        }
        if let Some(d) = self.doc_of(c) {
            f.insert("doc".to_string(), Fact::Str(d));
        }
        let (name, ty, uses): (String, String, Option<(&'static str, String)>);
        if let Some(r) = attr(c, "ref") {
            let r = r.to_string();
            let Some(q) = self.resolve(&r, c.span) else {
                return;
            };
            name = q.local.clone();
            ty = q.key();
            uses = Some(("uses-element", q.key()));
        } else if let Some(n) = attr(c, "name") {
            name = n.to_string();
            match attr(c, "type") {
                Some(t) => {
                    let t = t.to_string();
                    let Some(q) = self.resolve(&t, c.span) else {
                        return;
                    };
                    match q.builtin() {
                        Some(w) => {
                            ty = w.to_string();
                            uses = None;
                        }
                        None => {
                            ty = q.key();
                            uses = Some(("uses-type", q.key()));
                        }
                    }
                }
                None => {
                    ty = "string".to_string();
                    uses = None;
                }
            }
        } else {
            return;
        }
        f.insert("field_type".to_string(), Fact::Str(ty));
        for st in self.children(c, "simpleType") {
            let pushed = self.push_scope(st);
            for r in self.children(st, "restriction") {
                let values: Vec<Fact> = self
                    .children(r, "enumeration")
                    .into_iter()
                    .filter_map(|e| attr(e, "value").map(s))
                    .collect();
                if !values.is_empty() {
                    f.insert("enum".to_string(), Fact::List(values));
                }
                if let Some(base) = attr(r, "base") {
                    let base = base.to_string();
                    if let Some(q) = self.resolve(&base, r.span) {
                        let t = q.builtin().map_or_else(|| q.key(), str::to_string);
                        f.insert("field_type".to_string(), Fact::Str(t));
                    }
                }
            }
            if pushed {
                self.scopes.pop();
            }
        }
        let idx = self.b.symbol(Some(owner), "field", &name, c.span, f);
        self.b.export(idx, MEMBER, format!("{owner_key}/{name}"));
        if let Some((kind, key)) = uses {
            // An attribute `ref` names a global attribute, which is not exported; a typed
            // attribute names a simple type.
            if kind == "uses-type" {
                self.b.reference(
                    idx,
                    kind,
                    vec![ExportKey {
                        namespace: TYPE.to_string(),
                        key,
                    }],
                    c.span,
                    Confidence::Extracted,
                );
            }
        }
    }
}
