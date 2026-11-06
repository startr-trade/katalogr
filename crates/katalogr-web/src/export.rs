//! The single-file export (FEAT-034): the plan of calls a file must hold, the key each answer is
//! stored under, and the one HTML file that holds the page, its scripts and the answers.
//!
//! Pure functions over strings and values: no index is read and no file is written here. The
//! wrapper runs the plan through the API's op table and hands the answers to [`single_file`].

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt::Write;

use serde::Serialize;

pub use katalogr_query::PositionRef;

/// One call the file must hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedCall {
    pub op: String,
    pub params: BTreeMap<String, String>,
}

impl PlannedCall {
    fn new(op: &str, params: &[(&str, &str)]) -> PlannedCall {
        PlannedCall {
            op: op.to_string(),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

/// What the export asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanOptions {
    pub with_source: bool,
}

/// The calls every export makes: from the manifest, each ready panel of a view whose `needs`
/// is in `capabilities`, plus the graph and service-map calls: `views/structure` as the page
/// asks for it by default and with `level=package` and `level=file`, `views/cycles`,
/// `views/contracts` and `views/`, the catalogue. Without duplicates, in a fixed order.
pub fn plan_views(capabilities: &[String]) -> Vec<PlannedCall> {
    let have = |needs: &Option<String>| match needs {
        Some(n) => capabilities.iter().any(|c| c == n),
        None => true,
    };
    let mut out: Vec<PlannedCall> = Vec::new();
    for view in crate::manifest::manifest().views {
        if !have(&view.needs) {
            continue;
        }
        for panel in view.panels {
            if !have(&panel.needs) {
                continue;
            }
            if let Some(op) = panel.op {
                out.push(PlannedCall {
                    op,
                    params: panel.params,
                });
            }
        }
    }
    out.push(PlannedCall::new("views/", &[]));
    out.push(PlannedCall::new("views/structure", &[]));
    out.push(PlannedCall::new("views/structure", &[("level", "package")]));
    out.push(PlannedCall::new("views/structure", &[("level", "file")]));
    out.push(PlannedCall::new("views/cycles", &[]));
    out.push(PlannedCall::new("views/contracts", &[]));
    let mut seen = BTreeSet::new();
    out.retain(|c| seen.insert(answer_key(c)));
    out
}

/// The calls for one symbol's page, from its explain answer's path and line: `explain`, `why`
/// at `path:line`, `tests`, the callers view, and `show` with `signature=true` unless
/// `opts.with_source` is set. When `capabilities` holds `type-lineage`, also `lineage` (`text`)
/// and the lineage view (`symbol`), so an embedded symbol keeps its lineage offline.
pub fn plan_symbol(
    name: &str,
    path: &str,
    line: u32,
    opts: &PlanOptions,
    capabilities: &[String],
) -> Vec<PlannedCall> {
    let location = format!("{path}:{line}");
    let mut show = vec![("symbol", name)];
    if !opts.with_source {
        show.push(("signature", "true"));
    }
    let mut calls = vec![
        PlannedCall::new("explain", &[("symbol", name)]),
        PlannedCall::new("why", &[("location", &location)]),
        PlannedCall::new("tests", &[("text", name)]),
        PlannedCall::new("views/callers", &[("symbol", name)]),
        PlannedCall::new("show", &show),
    ];
    if capabilities.iter().any(|c| c == "type-lineage") {
        calls.push(PlannedCall::new("lineage", &[("text", name)]));
        calls.push(PlannedCall::new("views/lineage", &[("symbol", name)]));
    }
    calls
}

/// What JavaScript's `encodeURIComponent` leaves alone: `A-Za-z0-9-_.!~*'()`.
fn encode_component(s: &str, out: &mut String) {
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
}

/// `<op>?<name>=<value>&…`, names sorted, values encoded like JS `encodeURIComponent`.
/// Equal to `KW.core.apiPath(op, params)` without its `/api/v1/` prefix, for every input: an
/// empty value is left out, as the page leaves it out, and names sort by UTF-16 code unit, as
/// JavaScript sorts them.
pub fn answer_key(call: &PlannedCall) -> String {
    let mut names: Vec<&String> = call
        .params
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, _)| k)
        .collect();
    names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    let mut out = call.op.clone();
    for (i, name) in names.iter().enumerate() {
        out.push(if i == 0 { '?' } else { '&' });
        encode_component(name, &mut out);
        out.push('=');
        encode_component(&call.params[*name], &mut out);
    }
    out
}

/// Removes the top-level `watermark`, `context` and `pending`: the fields that vary between
/// runs.
pub fn strip_volatile(body: &mut serde_json::Value) {
    if let Some(map) = body.as_object_mut() {
        for k in ["watermark", "context", "pending"] {
            map.remove(k);
        }
    }
}

/// One stored answer.
#[derive(Clone, Debug, Serialize)]
pub struct EmbeddedAnswer {
    pub status: u16,
    pub body: serde_json::Value,
}

/// The file's data.
#[derive(Clone, Debug, Serialize)]
pub struct ExportData {
    /// 1
    pub format: u32,
    /// The crate version.
    pub katalogr: String,
    pub with_source: bool,
    pub capabilities: Vec<String>,
    pub positions: Vec<PositionRef>,
    /// Sorted.
    pub repos: Vec<String>,
    /// `SymbolHit`, in plan order.
    pub symbols: Vec<serde_json::Value>,
    /// `Partial` entries for what was cut.
    pub partial: Vec<serde_json::Value>,
    /// By [`answer_key`].
    pub answers: BTreeMap<String, EmbeddedAnswer>,
}

/// True when every answer in the run reflects the same positions.
pub fn positions_agree(seen: &[Vec<PositionRef>]) -> bool {
    let norm = |v: &Vec<PositionRef>| {
        let mut v = v.clone();
        v.sort();
        v.dedup();
        v
    };
    match seen.split_first() {
        None => true,
        Some((first, rest)) => {
            let first = norm(first);
            rest.iter().all(|p| norm(p) == first)
        }
    }
}

/// The policy the file sets in a meta tag: nothing may load, so a browser that honours it
/// refuses any request. There is no `connect-src`, since `default-src 'none'` covers it.
pub const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'";

/// Scripts of the file after the data, in order. `kw-static.js` comes first.
const SCRIPTS: [&str; 8] = [
    "kw-static.js",
    "cytoscape.min.js",
    "kw-core.js",
    "kw-dom.js",
    "kw-diagram.js",
    "kw-explore.js",
    "kw-views.js",
    "kw-main.js",
];

/// `</script` (in any case) would end the element early: `<\/script` means the same inside a
/// script, and the text ends with a newline.
fn script_safe(js: &str) -> String {
    let lower = js.to_ascii_lowercase();
    let mut out = String::with_capacity(js.len() + 1);
    let mut from = 0;
    while let Some(i) = lower[from..].find("</script") {
        let at = from + i;
        out.push_str(&js[from..at]);
        out.push_str("<\\/script");
        from = at + "</script".len();
    }
    out.push_str(&js[from..]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// A JSON text safe inside a `<script type="application/json">`: every `<` is `<`.
fn json_safe(json: &str) -> String {
    json.replace('<', "\\u003c")
}

fn asset_text(name: &str) -> &'static str {
    crate::asset(name)
        .unwrap_or_else(|| panic!("asset {name} is embedded"))
        .text
}

/// The whole file. Inlines every asset. `<` in the data is written `<`, and `</script` in
/// a script as `<\/script`. Panics only if an asset holds `</style`, which a test rules out.
pub fn single_file(data: &ExportData) -> String {
    let theme = asset_text(katalogr_render::assets::PELE_THEME_CSS_NAME);
    let css = asset_text("app.css");
    assert!(
        !css.to_ascii_lowercase().contains("</style")
            && !theme.to_ascii_lowercase().contains("</style"),
        "a stylesheet holds `</style`"
    );
    let head = format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"{CSP}\">\n\
         <link rel=\"icon\" href=\"data:,\">\n{}<style>\n{css}</style>",
        katalogr_render::assets::head_tags(None),
    );
    let json = serde_json::to_string(data).expect("export data serialises");
    let manifest = format!(
        "<script type=\"application/json\" id=\"kw-views\">{}</script>\n\
         <script type=\"application/json\" id=\"kw-data\">{}</script>",
        json_safe(crate::VIEWS_JSON.trim_end()),
        json_safe(&json),
    );
    let scripts: String = SCRIPTS
        .iter()
        .map(|n| format!("<script>\n{}</script>", script_safe(asset_text(n))))
        .collect::<Vec<_>>()
        .join("\n");
    crate::INDEX_HTML
        .replace("<!--KW:API-->", "")
        .replace("<!--KW:HEAD-->", &head)
        .replace("<!--KW:MANIFEST-->", &manifest)
        .replace("<!--KW:SCRIPTS-->", &scripts)
}
