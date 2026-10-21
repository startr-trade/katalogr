//! The tool table, input schemas, argument parsing, the one facade call per tool and the MCP
//! result for an answer (ADR-0023, FEAT-067).
//!
//! No query logic lives here: every tool resolves the project as one CLI invocation does,
//! makes exactly one `Katalogr` call and renders it with the CLI's JSON renderer.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use katalogr_dist::{DistError, Katalogr, Options};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, MetaObject, Tool};
use serde_json::{Value, json};

use crate::exit::ExitCode;
use crate::mcp::ServeConfig;
use crate::render::json::{self as render_json, JsonAnswer};

/// The type of one tool argument.
#[derive(Debug, Clone, Copy)]
pub enum Ty {
    Str,
    /// An integer of at least this value.
    Int(i64),
    Bool,
    /// One of these strings.
    Enum(&'static [&'static str]),
}

/// One tool argument; the arguments and defaults equal the CLI flags.
#[derive(Debug, Clone, Copy)]
pub struct Arg {
    pub name: &'static str,
    pub ty: Ty,
    pub required: bool,
    /// The default of an integer argument, shown in the schema.
    pub default: Option<i64>,
}

const fn req(name: &'static str, ty: Ty) -> Arg {
    Arg {
        name,
        ty,
        required: true,
        default: None,
    }
}

const fn opt(name: &'static str, ty: Ty) -> Arg {
    Arg {
        name,
        ty,
        required: false,
        default: None,
    }
}

const fn opt_int(name: &'static str, min: i64, default: i64) -> Arg {
    Arg {
        name,
        ty: Ty::Int(min),
        required: false,
        default: Some(default),
    }
}

/// The `budget` argument every query tool takes.
const BUDGET: Arg = opt("budget", Ty::Int(1));

/// A tool: its name, a description of at most 100 characters and its arguments.
#[derive(Debug, Clone, Copy)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    /// A query tool: it takes `budget` and its answer has a `schema/v1` output schema.
    pub query: bool,
    pub args: &'static [Arg],
}

/// Fixed order: find, refs, callers, explain, show, path, map, search, dangling, why, index,
/// status.
pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "find",
        description: "Symbols whose name matches the text.",
        query: true,
        args: &[req("text", Ty::Str)],
    },
    ToolSpec {
        name: "refs",
        description: "What references a symbol, including through re-exports.",
        query: true,
        args: &[req("symbol", Ty::Str), opt("expand", Ty::Bool)],
    },
    ToolSpec {
        name: "callers",
        description: "Callers of a symbol, up to depth hops.",
        query: true,
        args: &[
            req("symbol", Ty::Str),
            opt_int("depth", 1, 1),
            opt("expand", Ty::Bool),
        ],
    },
    ToolSpec {
        name: "explain",
        description: "A symbol's signature, doc, links and rationale.",
        query: true,
        args: &[req("symbol", Ty::Str), opt_int("per_side", 0, 10)],
    },
    ToolSpec {
        name: "show",
        description: "A symbol's source lines, or only its signature and doc.",
        query: true,
        args: &[req("symbol", Ty::Str), opt("signature", Ty::Bool)],
    },
    ToolSpec {
        name: "path",
        description: "Shortest chain of links between two symbols.",
        query: true,
        args: &[
            req("from", Ty::Str),
            req("to", Ty::Str),
            opt_int("max_len", 1, 6),
        ],
    },
    ToolSpec {
        name: "map",
        description: "Project outline: symbols ranked by PageRank.",
        query: true,
        args: &[],
    },
    ToolSpec {
        name: "search",
        description: "Symbols ranked by BM25 over names, docs, sections and contract keys.",
        query: true,
        args: &[req("terms", Ty::Str), opt_int("limit", 1, 20)],
    },
    ToolSpec {
        name: "dangling",
        description: "References that name something that does not exist.",
        query: true,
        args: &[opt(
            "kind",
            Ty::Enum(&["links-to", "annotated-by", "refs-item", "contract"]),
        )],
    },
    ToolSpec {
        name: "why",
        description: "The symbol at FILE:LINE and the rationale linked to it.",
        query: true,
        args: &[req("location", Ty::Str)],
    },
    ToolSpec {
        name: "index",
        description: "Queue changes and bring the index up to date.",
        query: false,
        args: &[
            opt("since", Ty::Str),
            opt("range", Ty::Str),
            opt("full", Ty::Bool),
            opt("history", Ty::Bool),
        ],
    },
    ToolSpec {
        name: "status",
        description: "Index watermark, pending changes and staleness.",
        query: false,
        args: &[],
    },
];

/// The name is in [`TOOLS`].
pub fn is_tool(name: &str) -> bool {
    spec_of(name).is_some()
}

fn spec_of(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|t| t.name == name)
}

fn owned(s: Option<&str>) -> Option<String> {
    s.map(str::to_string)
}

/// A name that is not in [`TOOLS`].
#[derive(Debug, PartialEq, Eq)]
pub struct UnknownTool;

fn arg_schema(a: &Arg) -> Value {
    match a.ty {
        Ty::Str => json!({"type": "string"}),
        Ty::Int(min) => match a.default {
            Some(d) => json!({"type": "integer", "minimum": min, "default": d}),
            None => json!({"type": "integer", "minimum": min}),
        },
        Ty::Bool => json!({"type": "boolean"}),
        Ty::Enum(values) => json!({"type": "string", "enum": values}),
    }
}

/// The tool's arguments: its own, then `budget` for a query tool.
fn all_args(spec: &ToolSpec) -> Vec<Arg> {
    let mut v = spec.args.to_vec();
    if spec.query {
        v.push(BUDGET);
    }
    v
}

fn input_schema(spec: &ToolSpec) -> JsonObject {
    let args = all_args(spec);
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for a in &args {
        properties.insert(a.name.to_string(), arg_schema(a));
        if a.required {
            required.push(Value::from(a.name));
        }
    }
    let mut schema = serde_json::Map::new();
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), Value::Array(required));
    }
    schema.insert("additionalProperties".into(), json!(false));
    schema
}

/// The `schema/v1` file of a query tool's answer, embedded in the binary.
fn output_schema(name: &str) -> Option<JsonObject> {
    let text = match name {
        "find" => include_str!("../../../../schema/v1/find.schema.json"),
        "refs" => include_str!("../../../../schema/v1/refs.schema.json"),
        "callers" => include_str!("../../../../schema/v1/callers.schema.json"),
        "explain" => include_str!("../../../../schema/v1/explain.schema.json"),
        "show" => include_str!("../../../../schema/v1/show.schema.json"),
        "path" => include_str!("../../../../schema/v1/path.schema.json"),
        "map" => include_str!("../../../../schema/v1/map.schema.json"),
        "search" => include_str!("../../../../schema/v1/search.schema.json"),
        "dangling" => include_str!("../../../../schema/v1/dangling.schema.json"),
        "why" => include_str!("../../../../schema/v1/why.schema.json"),
        _ => return None,
    };
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// The tools in [`TOOLS`] order. With `output_schemas` and a client that understands them,
/// each query tool carries its `schema/v1` file as `outputSchema`.
pub fn list(output_schemas: bool, structured: bool) -> Vec<Tool> {
    let mut tools = Vec::new();
    for spec in TOOLS {
        let mut tool = Tool::new(spec.name, spec.description, Arc::new(input_schema(spec)));
        if output_schemas
            && structured
            && spec.query
            && let Some(schema) = output_schema(spec.name)
        {
            tool = tool.with_raw_output_schema(Arc::new(schema));
        }
        tools.push(tool);
    }
    tools
}

/// Whether the client's protocol version carries `structuredContent` and `outputSchema`
/// (`None` means the version is not known yet, so yes; else `v >= "2025-06-18"`, and ISO
/// dates compare lexically).
pub fn has_structured(version: Option<&str>) -> bool {
    match version {
        None => true,
        Some(v) => v >= "2025-06-18",
    }
}

/// A usage error: nothing on stdout, the message, exit 2.
fn usage(message: String) -> JsonAnswer {
    JsonAnswer {
        exit: ExitCode::Usage,
        stdout: None,
        message: Some(message),
        warnings: Vec::new(),
        diagnostic: Vec::new(),
    }
}

/// Arguments checked against a tool's schema. `rmcp` does not validate them, so this is the
/// check that `additionalProperties: false`, the types and the minimums hold.
pub(crate) struct Parsed {
    values: BTreeMap<&'static str, Value>,
}

impl Parsed {
    pub(crate) fn parse(spec: &ToolSpec, args: &Value) -> Result<Parsed, String> {
        let tool = spec.name;
        let empty = serde_json::Map::new();
        let given = match args {
            Value::Null => &empty,
            Value::Object(m) => m,
            _ => return Err(format!("{tool}: arguments: expected object")),
        };
        let known = all_args(spec);
        for key in given.keys() {
            if !known.iter().any(|a| a.name == key) {
                return Err(format!("{tool}: {key}: unknown argument"));
            }
        }
        let mut values = BTreeMap::new();
        for a in &known {
            let key = a.name;
            let Some(v) = given.get(key) else {
                if a.required {
                    return Err(format!("{tool}: {key}: required"));
                }
                continue;
            };
            match a.ty {
                Ty::Str if !v.is_string() => {
                    return Err(format!("{tool}: {key}: expected string"));
                }
                Ty::Bool if !v.is_boolean() => {
                    return Err(format!("{tool}: {key}: expected boolean"));
                }
                Ty::Int(min) => {
                    let Some(n) = v.as_i64() else {
                        return Err(format!("{tool}: {key}: expected integer"));
                    };
                    if n < min {
                        return Err(format!("{tool}: {key}: must be >= {min}"));
                    }
                    if u32::try_from(n).is_err() {
                        return Err(format!("{tool}: {key}: must be <= {}", u32::MAX));
                    }
                }
                Ty::Enum(allowed) => {
                    let ok = v.as_str().is_some_and(|s| allowed.contains(&s));
                    if !ok {
                        return Err(format!(
                            "{tool}: {key}: expected one of {}",
                            allowed.join(", ")
                        ));
                    }
                }
                _ => {}
            }
            values.insert(key, v.clone());
        }
        Ok(Parsed { values })
    }

    fn text(&self, key: &str) -> Option<&str> {
        self.values.get(key).and_then(Value::as_str)
    }

    fn need(&self, key: &str) -> &str {
        self.text(key).unwrap_or("")
    }

    fn flag(&self, key: &str) -> bool {
        self.values
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn int(&self, key: &str, default: u32) -> u32 {
        self.values
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(default)
    }

    fn budget(&self) -> Option<u64> {
        self.values.get("budget").and_then(Value::as_u64)
    }
}

/// The `index` conflicts of the CLI: `since` with `range` or `full`, and `range` with `full`
/// or `history`.
fn index_conflict(p: &Parsed) -> Option<String> {
    let set = |k: &str| p.values.contains_key(k) && (p.text(k).is_some() || p.flag(k));
    let pairs = [
        ("since", "range"),
        ("since", "full"),
        ("range", "full"),
        ("range", "history"),
    ];
    for (a, b) in pairs {
        if set(a) && set(b) {
            return Some(format!("index: {a} conflicts with {b}"));
        }
    }
    None
}

fn answer<R>(r: Result<R, DistError>, render: impl FnOnce(R) -> JsonAnswer) -> JsonAnswer {
    match r {
        Ok(x) => render(x),
        Err(e) => render_json::error(&e),
    }
}

/// Run one tool synchronously: validate the arguments, resolve the project, make the one
/// facade call and render it with the CLI's JSON renderer. Never panics on bad input; every
/// failure is a [`JsonAnswer`].
pub fn call(cfg: &ServeConfig, name: &str, args: &Value) -> Result<JsonAnswer, UnknownTool> {
    let Some(spec) = spec_of(name) else {
        return Err(UnknownTool);
    };
    let p = match Parsed::parse(spec, args) {
        Ok(p) => p,
        Err(message) => return Ok(usage(message)),
    };
    if name == "index"
        && let Some(message) = index_conflict(&p)
    {
        return Ok(usage(message));
    }
    let k = match Katalogr::resolve(&cfg.root, &cfg.overrides) {
        Ok(k) => k,
        Err(e) => return Ok(render_json::error(&e)),
    };
    let budget = p.budget().or(cfg.budget);
    let opts = &cfg.options;
    let out = match name {
        "find" => answer(Katalogr::find(&k, p.need("text"), opts), |r| {
            render_json::list(r, budget, "find")
        }),
        "refs" => {
            let opts = &Options {
                expand: p.flag("expand"),
                ..opts.clone()
            };
            answer(Katalogr::refs(&k, p.need("symbol"), opts), |r| {
                render_json::list_folded(r, budget, "refs")
            })
        }
        "callers" => {
            let opts = &Options {
                expand: p.flag("expand"),
                ..opts.clone()
            };
            answer(
                Katalogr::callers(&k, p.need("symbol"), p.int("depth", 1), opts),
                |r| render_json::list_folded(r, budget, "callers"),
            )
        }
        "explain" => answer(
            Katalogr::explain(&k, p.need("symbol"), p.int("per_side", 10) as usize, opts),
            |r| render_json::explain(r, budget),
        ),
        "show" => answer(
            Katalogr::show(&k, p.need("symbol"), p.flag("signature"), opts),
            |r| render_json::show(r, budget),
        ),
        "path" => answer(
            Katalogr::path(&k, p.need("from"), p.need("to"), p.int("max_len", 6), opts),
            |r| render_json::list(r, budget, "path"),
        ),
        "map" => answer(Katalogr::map(&k, opts), |r| {
            render_json::list(r, budget, "map")
        }),
        "search" => answer(
            Katalogr::search(&k, p.need("terms"), p.int("limit", 20) as usize, opts),
            |r| render_json::list(r, budget, "search"),
        ),
        "dangling" => {
            let kinds: Vec<&str> = p.text("kind").into_iter().collect();
            answer(Katalogr::dangling(&k, &kinds, opts), |r| {
                render_json::dangling(r, budget)
            })
        }
        "why" => answer(Katalogr::why(&k, p.need("location"), opts), |r| {
            render_json::why(r, budget)
        }),
        "index" => {
            let since = owned(p.text("since"));
            let range = owned(p.text("range"));
            let (files, hist) =
                crate::cmd::index::scope(since.as_ref(), range.as_ref(), p.flag("history"));
            answer(
                Katalogr::index(&k, files, p.flag("full"), hist.as_ref(), opts),
                |r| render_json::report(&r),
            )
        }
        "status" => answer(Katalogr::status(&k, opts), |r| render_json::report(&r)),
        _ => return Err(UnknownTool),
    };
    Ok(out)
}

/// An answer is a tool error when the CLI would have reported a failure and printed no
/// answer: ambiguity, no match, bad usage, or a failure with nothing on stdout. `dangling`
/// findings and a stale answer are answers, not failures.
fn is_error(a: &JsonAnswer) -> bool {
    match a.exit {
        ExitCode::Ambiguous | ExitCode::NoMatch | ExitCode::Usage => true,
        ExitCode::Error | ExitCode::Stale => a.stdout.is_none(),
        ExitCode::Ok => false,
    }
}

/// The MCP `CallToolResult` for an answer. `content[last].text` is the CLI's stdout line (or
/// the message when there is none); `_meta["katalogr/exit"]` is the CLI exit code.
pub fn to_call_result(a: &JsonAnswer, structured: bool) -> CallToolResult {
    let is_error = is_error(a);
    let mut content = Vec::new();
    if let Some(m) = &a.message
        && a.stdout.is_some()
    {
        content.push(ContentBlock::text(m.clone()));
    }
    match (&a.stdout, &a.message) {
        (Some(out), _) => content.push(ContentBlock::text(out.clone())),
        (None, Some(m)) => content.push(ContentBlock::text(m.clone())),
        (None, None) => content.push(ContentBlock::text(String::new())),
    }
    let mut result = if is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    };
    if structured
        && !is_error
        && let Some(out) = &a.stdout
        && let Ok(v) = serde_json::from_str::<Value>(out)
    {
        result.structured_content = Some(v);
    }
    let mut meta = serde_json::Map::new();
    meta.insert("katalogr/exit".into(), json!(a.exit.code()));
    if !a.warnings.is_empty() {
        meta.insert("katalogr/warnings".into(), json!(a.warnings));
    }
    result.meta = Some(MetaObject(meta));
    result
}

/// A panic payload from a blocking call as an answer: exit Error, `internal error: <payload>`.
pub fn panic_answer(payload: Box<dyn Any + Send>) -> JsonAnswer {
    let p = match payload.downcast::<String>() {
        Ok(s) => *s,
        Err(other) => match other.downcast::<&'static str>() {
            Ok(s) => (*s).to_string(),
            Err(_) => "panic".to_string(),
        },
    };
    JsonAnswer {
        exit: ExitCode::Error,
        stdout: None,
        message: Some(format!("internal error: {p}")),
        warnings: Vec::new(),
        diagnostic: Vec::new(),
    }
}
