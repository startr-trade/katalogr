//! Reading an OpenAPI 3.x or Swagger 2.0 document from the positioned tree of `katalogr-ext-data`
//! (FEAT-014 section 4): its version, base paths and operations, in document order.

use crate::route;
use katalogr_ext_data::tree::{Entry, Node};
use katalogr_model::{Fact, Span};
use std::collections::BTreeMap;

/// The path-item keys that are operations, case-sensitive.
pub const OPERATION_METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Longest `doc` fact kept, in bytes.
pub const DOC_MAX: usize = 200;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Version {
    V3,
    V2,
}

/// One operation of a document.
#[derive(Clone, Debug)]
pub struct Op {
    /// Upper-case method.
    pub method: String,
    /// The path template as written.
    pub path: String,
    /// From the method key to the end of the operation value.
    pub span: Span,
    /// The method key.
    pub key_span: Span,
    pub facts: BTreeMap<String, Fact>,
    /// The normalised route key, when the method and path normalise.
    pub route: Option<String>,
}

/// `openapi: 3.x` gives V3, `swagger: 2.0` gives V2.
pub fn kind(root: &Node) -> Option<Version> {
    if !root.is_map() {
        return None;
    }
    if root
        .get("openapi")
        .and_then(Node::as_text)
        .is_some_and(|v| v.starts_with("3."))
    {
        return Some(Version::V3);
    }
    (root.get("swagger").and_then(Node::as_text) == Some("2.0")).then_some(Version::V2)
}

/// `openapi: 3.x` or `swagger: 2.0` text.
pub fn version_text(root: &Node) -> Option<&str> {
    root.get("openapi")
        .or_else(|| root.get("swagger"))
        .and_then(Node::as_text)
}

/// The path part of the first server of `holder` (`servers[0]`), variables replaced by their
/// defaults when scalar. `None` when `holder` has no server with a url.
fn server_base(holder: &Node) -> Option<String> {
    let server = holder.get("servers")?.items().first()?;
    let mut url = server.get("url")?.as_text()?.to_string();
    if let Some(vars) = server.get("variables") {
        for e in vars.entries() {
            if let Some(d) = e.value.get("default").and_then(Node::as_text) {
                url = url.replace(&format!("{{{}}}", e.key), d);
            }
        }
    }
    Some(route::base_of(&url))
}

/// The base path of an operation: for V3 the operation's `servers[0]`, else its path item's,
/// else the root's; for V2 the root `basePath`.
pub fn base_path(version: Version, root: &Node, path_item: &Node, op: &Node) -> String {
    match version {
        Version::V3 => server_base(op)
            .or_else(|| server_base(path_item))
            .or_else(|| server_base(root))
            .unwrap_or_default(),
        Version::V2 => root_base(version, root),
    }
}

/// The base path of the document root.
pub fn root_base(version: Version, root: &Node) -> String {
    match version {
        Version::V3 => server_base(root).unwrap_or_default(),
        Version::V2 => root
            .get("basePath")
            .and_then(Node::as_text)
            .map(route::base_of)
            .unwrap_or_default(),
    }
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

fn doc_of(op: &Node) -> Option<String> {
    let text = op
        .get("summary")
        .and_then(Node::as_text)
        .filter(|t| !t.trim().is_empty())
        .or_else(|| op.get("description").and_then(Node::as_text))?;
    let line = text.trim().lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return None;
    }
    let mut end = line.len().min(DOC_MAX);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    Some(line[..end].to_string())
}

/// The operations of `root`, in document order.
pub fn operations(root: &Node, version: Version) -> Vec<Op> {
    let mut out = Vec::new();
    let Some(paths) = root.get("paths") else {
        return out;
    };
    for pe in paths.entries() {
        if !pe.key.starts_with('/') || !pe.value.is_map() {
            continue;
        }
        for oe in pe.value.entries() {
            if !OPERATION_METHODS.contains(&oe.key.as_str()) {
                continue;
            }
            let method = oe.key.to_ascii_uppercase();
            let base = base_path(version, root, &pe.value, &oe.value);
            let route = route::normalise(&method, &format!("{base}{}", pe.key));
            let mut facts = BTreeMap::new();
            facts.insert("method".to_string(), Fact::Str(method.clone()));
            facts.insert("path".to_string(), Fact::Str(pe.key.clone()));
            if let Some(r) = &route {
                facts.insert("route".to_string(), Fact::Str(r.clone()));
            }
            if let Some(id) = oe.value.get("operationId").and_then(Node::as_text) {
                facts.insert("operation_id".to_string(), Fact::Str(id.to_string()));
            }
            if let Some(tags) = oe.value.get("tags") {
                let list: Vec<Fact> = tags
                    .items()
                    .iter()
                    .filter_map(|t| t.as_text().map(|t| Fact::Str(t.to_string())))
                    .collect();
                if !list.is_empty() {
                    facts.insert("tags".to_string(), Fact::List(list));
                }
            }
            if oe.value.get("deprecated").and_then(Node::as_text) == Some("true") {
                facts.insert("deprecated".to_string(), Fact::Bool(true));
            }
            if let Some(d) = doc_of(&oe.value) {
                facts.insert("doc".to_string(), Fact::Str(d));
            }
            out.push(Op {
                method,
                path: pe.key.clone(),
                span: entry_span(oe),
                key_span: oe.key_span,
                facts,
                route,
            });
        }
    }
    out
}
