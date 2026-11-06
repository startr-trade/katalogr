//! The op table: one row per route, one facade call per row, rendered with the CLI's JSON
//! renderer so the body is byte for byte what `--json` prints (FEAT-049 R-3, ADR-0023's pattern).
//!
//! No query logic lives here. A row's parameters have the CLI flags' names (kebab-case) and
//! defaults; the view-ID arguments are the CLI's, so one question has one view ID on both.

use std::collections::BTreeMap;
use std::path::Path;

use katalogr_dist::analytics::{
    CommunityOptions, CycleOptions, DeadCodeOptions, DeadConfidence, HubOptions,
};
use katalogr_dist::query::TestsOptions;
use katalogr_dist::{CoverageOptions, DistError, Katalogr, Options, Overrides, ViewQuery};

use super::http::{HttpError, capped_timeout};
use crate::cli::LevelArg;
use crate::exit::ExitCode;
use crate::render::json::{self as render_json, JsonAnswer};
use crate::render::{COMMUNITIES, COVERAGE, CYCLES, DEAD_CODE, DEAD_CODE_CHECK, HUBS, RATIONALE};

/// The type of one parameter.
#[derive(Debug, Clone, Copy)]
pub enum Ty {
    Str,
    /// An unsigned integer of at least this value.
    Int(u64),
    Float,
    Bool,
    /// One of these strings.
    Enum(&'static [&'static str]),
}

/// One query parameter. `default` is shown in the OpenAPI document and applied when absent.
#[derive(Debug, Clone, Copy)]
pub struct Param {
    pub name: &'static str,
    pub ty: Ty,
    pub required: bool,
    pub default: Option<&'static str>,
    pub help: &'static str,
}

const fn req(name: &'static str, help: &'static str) -> Param {
    Param {
        name,
        ty: Ty::Str,
        required: true,
        default: None,
        help,
    }
}

const fn opt(name: &'static str, ty: Ty, help: &'static str) -> Param {
    Param {
        name,
        ty,
        required: false,
        default: None,
        help,
    }
}

const fn dflt(name: &'static str, ty: Ty, default: &'static str, help: &'static str) -> Param {
    Param {
        name,
        ty,
        required: false,
        default: Some(default),
        help,
    }
}

/// The consistency modes, as the CLI spells them.
pub const MODES: &[&str] = &["wait", "wait-all", "stale-ok"];
const LEVELS: &[&str] = &["symbol", "file", "package"];

/// Parameters every op takes, each with the CLI's meaning.
pub const COMMON: &[Param] = &[
    opt(
        "context",
        Ty::Str,
        "`current`, `base` or an id from context-list",
    ),
    opt("at", Ty::Str, "answer for this position"),
    dflt(
        "consistency",
        Ty::Enum(MODES),
        "stale-ok",
        "freshness requirement",
    ),
    opt("timeout", Ty::Float, "seconds to wait, at most 120"),
    opt(
        "budget",
        Ty::Int(1),
        "limit the answer to about this many tokens",
    ),
    opt("diagnostic", Ty::Bool, "include a diagnostic trace"),
    opt("expand", Ty::Bool, "list every hit instead of folding"),
];

/// A request's parameters, validated, with defaults applied.
#[derive(Debug, Default)]
pub struct Args {
    values: BTreeMap<String, String>,
}

impl Args {
    /// Check `given` against `params` and [`COMMON`]: unknown names, missing required ones and
    /// bad values are all 400.
    pub fn parse(params: &[Param], given: &BTreeMap<String, String>) -> Result<Args, HttpError> {
        let names: Vec<&str> = params.iter().map(|p| p.name).collect();
        super::http::check_params(given, &names)?;
        let mut values = BTreeMap::new();
        for p in params.iter().chain(COMMON) {
            let Some(v) = given.get(p.name) else {
                if p.required {
                    return Err(HttpError::usage(format!("`{}` is required", p.name)));
                }
                if let Some(d) = p.default {
                    values.insert(p.name.to_string(), d.to_string());
                }
                continue;
            };
            let bad = |what: &str| HttpError::usage(format!("`{}`: expected {what}", p.name));
            let v = match p.ty {
                Ty::Str => v.clone(),
                Ty::Int(min) => match v.parse::<u64>() {
                    Ok(n) if n >= min && u32::try_from(n).is_ok() => n.to_string(),
                    _ => return Err(bad(&format!("an integer from {min} to {}", u32::MAX))),
                },
                Ty::Float => match v.parse::<f64>() {
                    Ok(f) if f.is_finite() && f >= 0.0 => {
                        if p.name == "timeout" {
                            capped_timeout(v)?.to_string()
                        } else {
                            v.clone()
                        }
                    }
                    _ => return Err(bad("a non-negative number")),
                },
                Ty::Bool => match v.as_str() {
                    "" | "true" | "1" => "true".to_string(),
                    "false" | "0" => "false".to_string(),
                    _ => return Err(bad("true or false")),
                },
                Ty::Enum(allowed) if allowed.contains(&v.as_str()) => v.clone(),
                Ty::Enum(allowed) => return Err(bad(&format!("one of {}", allowed.join(", ")))),
            };
            values.insert(p.name.to_string(), v);
        }
        Ok(Args { values })
    }

    pub fn opt(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn str(&self, key: &str) -> &str {
        self.opt(key).unwrap_or("")
    }

    pub fn flag(&self, key: &str) -> bool {
        self.opt(key) == Some("true")
    }

    pub fn int(&self, key: &str) -> u32 {
        self.opt(key).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    pub fn float(&self, key: &str) -> Option<f64> {
        self.opt(key).and_then(|v| v.parse().ok())
    }

    pub fn budget(&self) -> Option<u64> {
        self.opt("budget").and_then(|v| v.parse().ok())
    }

    /// The `consistency` parameter, as the facade names it.
    pub fn consistency(&self) -> katalogr_dist::Consistency {
        match self.opt("consistency") {
            Some("wait") => katalogr_dist::Consistency::Wait,
            Some("wait-all") => katalogr_dist::Consistency::WaitAll,
            _ => katalogr_dist::Consistency::StaleOk,
        }
    }

    fn level(&self) -> LevelArg {
        match self.str("level") {
            "file" => LevelArg::File,
            "package" => LevelArg::Package,
            _ => LevelArg::Symbol,
        }
    }
}

/// Where a call runs: the project is resolved for it, or the op resolves for itself.
#[derive(Clone, Copy)]
pub enum Call {
    /// The project is resolved first; a failure to resolve is the answer.
    Project(fn(&Katalogr, &Args, &Options) -> JsonAnswer),
    /// The op works without a resolved project (`doctor`, `position`): it gets the start
    /// directory and the overrides.
    Free(fn(&Path, &Overrides, &Args, &Options) -> JsonAnswer),
}

/// One route.
pub struct Op {
    /// The route segment and the `ViewQuery` op.
    pub name: &'static str,
    pub summary: &'static str,
    pub params: &'static [Param],
    /// The `schema/v1` file of the body (without `.schema.json`); `None` when the body has no
    /// schema file.
    pub schema: Option<&'static str>,
    pub call: Call,
}

fn answer<R>(r: Result<R, DistError>, render: impl FnOnce(R) -> JsonAnswer) -> JsonAnswer {
    match r {
        Ok(x) => render(x),
        Err(e) => render_json::error(&e),
    }
}

/// The options of a question: the request's own, plus the view the ID is computed from.
fn q(base: &Options, a: &Args, op: &str, args: &[(&str, String)]) -> Options {
    let mut view = ViewQuery {
        op: op.to_string(),
        args: args
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    };
    if let Some(b) = a.budget() {
        view.args.insert("budget".to_string(), b.to_string());
    }
    Options {
        view: Some(view),
        diagnostic: a.flag("diagnostic"),
        ..base.clone()
    }
}

fn find(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let text = a.str("text");
    let o = q(o, a, "find", &[("text", text.to_string())]);
    answer(k.find(text, &o), |r| {
        render_json::list(r, a.budget(), "find")
    })
}

fn refs(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let (text, expand) = (a.str("text"), a.flag("expand"));
    let o = Options {
        expand,
        ..q(
            o,
            a,
            "refs",
            &[("text", text.to_string()), ("expand", expand.to_string())],
        )
    };
    answer(k.refs(text, &o), |r| {
        render_json::list_folded(r, a.budget(), "refs")
    })
}

fn callers(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let (text, depth, expand) = (a.str("text"), a.int("depth"), a.flag("expand"));
    let o = Options {
        expand,
        ..q(
            o,
            a,
            "callers",
            &[
                ("text", text.to_string()),
                ("depth", depth.to_string()),
                ("expand", expand.to_string()),
            ],
        )
    };
    answer(k.callers(text, depth, &o), |r| {
        render_json::list_folded(r, a.budget(), "callers")
    })
}

fn tests(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let (text, depth, cases) = (a.str("text"), a.int("depth"), a.flag("cases"));
    let topts = TestsOptions {
        depth,
        cases_only: cases,
    };
    let o = q(
        o,
        a,
        "tests",
        &[
            ("text", text.to_string()),
            ("depth", depth.to_string()),
            ("cases", cases.to_string()),
        ],
    );
    answer(k.tests(text, topts, &o), |r| {
        render_json::list(r, a.budget(), "tests")
    })
}

fn why(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let location = a.str("location");
    let limit = k.why_use_limit();
    let o = q(
        o,
        a,
        "why",
        &[
            ("location", location.to_string()),
            ("use_limit", limit.to_string()),
        ],
    );
    answer(k.why(location, Some(limit), &o), |r| {
        render_json::why(r, a.budget())
    })
}

fn explain(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let (symbol, per_side) = (a.str("symbol"), a.int("per-side") as usize);
    let o = q(
        o,
        a,
        "explain",
        &[
            ("symbol", symbol.to_string()),
            ("per_side", per_side.to_string()),
        ],
    );
    answer(k.explain(symbol, per_side, &o), |r| {
        render_json::explain(r, a.budget())
    })
}

fn show(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let (symbol, signature) = (a.str("symbol"), a.flag("signature"));
    let o = q(
        o,
        a,
        "show",
        &[
            ("symbol", symbol.to_string()),
            ("signature", signature.to_string()),
        ],
    );
    answer(k.show(symbol, signature, &o), |r| {
        render_json::show(r, a.budget())
    })
}

fn path(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let (from, to, max_len) = (a.str("from"), a.str("to"), a.int("max-len"));
    let o = q(
        o,
        a,
        "path",
        &[
            ("from", from.to_string()),
            ("to", to.to_string()),
            ("max_len", max_len.to_string()),
        ],
    );
    answer(k.path(from, to, max_len, &o), |r| {
        render_json::list(r, a.budget(), "path")
    })
}

fn map(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let o = q(o, a, "map", &[]);
    answer(k.map(&o), |r| render_json::list(r, a.budget(), "map"))
}

fn search(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let (terms, limit) = (a.str("terms"), a.int("limit") as usize);
    let o = q(
        o,
        a,
        "search",
        &[("terms", terms.to_string()), ("limit", limit.to_string())],
    );
    answer(k.search(terms, limit, &o), |r| {
        render_json::list(r, a.budget(), "search")
    })
}

/// A repeatable CLI flag is a comma-separated value here.
fn kinds(a: &Args) -> Vec<&str> {
    a.opt("kind")
        .map(|v| v.split(',').filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

fn dangling(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let kinds = kinds(a);
    let o = q(o, a, "dangling", &[("kinds", kinds.join(","))]);
    answer(k.dangling(&kinds, &o), |r| {
        render_json::dangling(r, a.budget())
    })
}

fn deps(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let package = a.opt("package").filter(|s| !s.is_empty());
    let (limit, used_by_limit) = (a.int("limit") as usize, a.int("used-by-limit") as usize);
    let dopts = katalogr_dist::DepsOptions {
        package: package.map(str::to_string),
        limit,
        used_by_limit,
    };
    let o = q(
        o,
        a,
        "deps",
        &crate::cmd::deps::args(package, limit, used_by_limit),
    );
    answer(k.deps(&dopts, &o), |r| render_json::list(r, a.budget(), ""))
}

fn mirrors(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let kinds = kinds(a);
    let o = q(o, a, "mirrors", &[("kinds", kinds.join(","))]);
    answer(k.mirrors(&kinds, &o), |r| {
        render_json::mirrors(r, a.budget())
    })
}

fn status(k: &Katalogr, _a: &Args, o: &Options) -> JsonAnswer {
    answer(k.status(o), |r| render_json::status(&r, k.context_name()))
}

fn docs_affected(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let since = a.opt("since").filter(|s| !s.is_empty());
    let kinds: Vec<String> = kinds(a).into_iter().map(str::to_string).collect();
    let limit = a.int("limit") as usize;
    let params = katalogr_dist::DocsAffectedParams {
        kinds: kinds.clone(),
        limit,
    };
    let o = q(
        o,
        a,
        "docs-affected",
        &crate::cmd::docs_affected::args(since, &kinds, limit),
    );
    answer(k.docs_affected(since, &params, &o), |r| {
        render_json::plain(r, |_| false)
    })
}

fn impact(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let nonempty = |key: &str| a.opt(key).filter(|s| !s.is_empty());
    let input = crate::cmd::impact::Input {
        symbol: nonempty("text"),
        file: nonempty("file"),
        range: nonempty("range"),
        since: nonempty("since"),
        item: nonempty("item"),
    };
    let (depth, limit, test_depth) = (a.int("depth"), a.int("limit") as usize, a.int("test-depth"));
    let facade = match input.parse(a.opt("at")) {
        Ok(f) => f,
        Err(e) => return render_json::error(&e),
    };
    let iopts = crate::cmd::impact::options(depth, limit, test_depth);
    let o = q(
        o,
        a,
        "impact",
        &crate::cmd::impact::args(&input, depth, limit, test_depth),
    );
    answer(k.impact(&facade, &iopts, &o), |r| {
        render_json::plain(r, |_| false)
    })
}

fn lineage(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let text = a.str("text");
    let (depth, limit, per_group) = (
        a.int("depth"),
        a.int("limit") as usize,
        a.int("per-group") as usize,
    );
    let expand = a.flag("expand");
    let o = Options {
        expand,
        ..q(
            o,
            a,
            "lineage",
            &crate::cmd::lineage::args(text, depth, limit, per_group, expand),
        )
    };
    answer(
        k.lineage(
            text,
            crate::cmd::lineage::options(depth, limit, per_group),
            &o,
        ),
        render_json::lineage,
    )
}

fn position(start: &Path, overrides: &Overrides, a: &Args, o: &Options) -> JsonAnswer {
    let repo = a.opt("repo");
    let result = match Katalogr::resolve(start, overrides) {
        Ok(k) => k.position(repo, o),
        Err(_) => katalogr_dist::position_without_board(start),
    };
    answer(result, |p| JsonAnswer {
        stdout: Some(serde_json::to_string(&p).expect("serialise position")),
        ..ok_answer()
    })
}

/// An answer with exit Ok and nothing else set.
fn ok_answer() -> JsonAnswer {
    JsonAnswer {
        exit: ExitCode::Ok,
        stdout: None,
        message: None,
        warnings: Vec::new(),
        diagnostic: Vec::new(),
        headers: Vec::new(),
    }
}

fn hubs(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let settings = k.insight_settings();
    let opts = HubOptions {
        graph: crate::cmd::graph_options(a.level(), !a.flag("strict"), &settings),
        min_degree: a.float("min-degree").unwrap_or(settings.hub_min_degree),
        limit: a.int("limit") as usize,
    };
    let o = q(o, a, "hubs", &[("options", format!("{opts:?}"))]);
    answer(k.hubs(&opts, &o), |r| {
        render_json::insight(r, a.budget(), "hubs", HUBS.fails)
    })
}

fn cycles(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let settings = k.insight_settings();
    let opts = CycleOptions {
        graph: crate::cmd::graph_options(a.level(), a.flag("include-inferred"), &settings),
        members: a.int("members") as usize,
        limit: a.int("limit") as usize,
    };
    let o = q(o, a, "cycles", &[("options", format!("{opts:?}"))]);
    answer(k.cycles(&opts, &o), |r| {
        render_json::insight(r, a.budget(), "cycles", CYCLES.fails)
    })
}

fn communities(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let settings = k.insight_settings();
    let opts = CommunityOptions {
        graph: crate::cmd::graph_options(a.level(), !a.flag("strict"), &settings),
        seed: a
            .opt("seed")
            .and_then(|v| v.parse().ok())
            .unwrap_or(settings.seed),
        resolution: a.float("resolution").unwrap_or(settings.resolution),
        min_size: settings.community_min_size,
        members: a.int("members") as usize,
        limit: a.int("limit") as usize,
        of: a.opt("of").map(str::to_string),
    };
    let o = q(o, a, "communities", &[("options", format!("{opts:?}"))]);
    answer(k.communities(&opts, &o), |r| {
        render_json::insight(r, a.budget(), "communities", COMMUNITIES.fails)
    })
}

fn dead_code(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let settings = k.insight_settings();
    let opts = DeadCodeOptions {
        code_kinds: BTreeMap::new(),
        exclude_kinds: settings.exclude_kinds,
        internal: a.flag("internal"),
        min_confidence: match a.str("min-confidence") {
            "high" => DeadConfidence::High,
            "low" => DeadConfidence::Low,
            _ => DeadConfidence::Medium,
        },
        path_prefix: a.opt("path").map(str::to_string),
        limit: a.int("limit") as usize,
    };
    let o = q(o, a, "dead-code", &[("options", format!("{opts:?}"))]);
    let fails = if a.flag("check") {
        DEAD_CODE_CHECK.fails
    } else {
        DEAD_CODE.fails
    };
    answer(k.dead_code(opts, &o), |r| {
        render_json::insight(r, a.budget(), "dead-code", fails)
    })
}

fn coverage(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let opts = CoverageOptions {
        repo: a.opt("repo").map(str::to_string),
        suffix_top: a.int("suffixes") as usize,
    };
    let o = q(o, a, "coverage", &[("options", format!("{opts:?}"))]);
    answer(k.coverage(&opts, &o), |r| {
        render_json::insight(r, a.budget(), "coverage", COVERAGE.fails)
    })
}

fn rationale(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let symbol = a.opt("symbol");
    let path = a.opt("path");
    let hubs = a.opt("hubs").and_then(|v| v.parse::<usize>().ok());
    let limit = a.int("limit") as usize;
    // The CLI refuses these pairs (clap `conflicts_with`).
    if symbol.is_some() && (path.is_some() || hubs.is_some()) || path.is_some() && hubs.is_some() {
        return render_json::error(&DistError::Usage(
            "rationale: symbol, path and hubs conflict".to_string(),
        ));
    }
    let o = q(
        o,
        a,
        "rationale",
        &[
            ("symbol", format!("{symbol:?}")),
            ("path", format!("{path:?}")),
            ("hubs", format!("{hubs:?}")),
            ("limit", limit.to_string()),
        ],
    );
    answer(k.rationale(symbol, path, hubs, limit, &o), |r| {
        render_json::insight(r, a.budget(), "rationale", RATIONALE.fails)
    })
}

fn doctor(start: &Path, overrides: &Overrides, a: &Args, _o: &Options) -> JsonAnswer {
    use katalogr_dist::doctor::{DoctorOptions, Severity, doctor};
    let opts = DoctorOptions {
        fail_on: match a.str("fail-on") {
            "error" => Severity::Error,
            _ => Severity::Warning,
        },
        skip_git: a.flag("skip-git"),
        verbose: a.flag("verbose"),
        ..DoctorOptions::default()
    };
    answer(doctor(start, overrides, &opts), |r| JsonAnswer {
        exit: if r.exit_failing() {
            ExitCode::Error
        } else {
            ExitCode::Ok
        },
        stdout: Some(serde_json::to_string(&r).expect("serialise report")),
        ..ok_answer()
    })
}

fn context_list(k: &Katalogr, _a: &Args, _o: &Options) -> JsonAnswer {
    answer(k.context_list(), |all| {
        let v: Vec<_> = all.iter().map(crate::cmd::context::to_json).collect();
        JsonAnswer {
            stdout: Some(serde_json::json!({ "schema": 1, "result": v }).to_string()),
            ..ok_answer()
        }
    })
}

fn context_diff(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    let (x, y, limit) = (a.str("a"), a.str("b"), a.int("limit") as usize);
    answer(k.context_diff(x, y, limit, o), |r| {
        render_json::plain(r, |_| false)
    })
}

fn context_conflicts(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    answer(k.context_conflicts(a.str("a"), a.str("b"), o), |r| {
        render_json::plain(r, |c| !c.conflicts.is_empty())
    })
}

const TEXT: Param = req("text", "canonical name, or part of a plain name");
const LIMIT20: Param = dflt("limit", Ty::Int(1), "20", "how many to list");
const LEVEL_SYMBOL: Param = dflt("level", Ty::Enum(LEVELS), "symbol", "node granularity");
const LEVEL_FILE: Param = dflt("level", Ty::Enum(LEVELS), "file", "node granularity");
const STRICT: Param = opt("strict", Ty::Bool, "leave out weak links");

macro_rules! op {
    ($name:expr, $summary:expr, $params:expr, $schema:expr, $call:expr) => {
        Op {
            name: $name,
            summary: $summary,
            params: $params,
            schema: $schema,
            call: $call,
        }
    };
}

/// Every route. Flows, checks and exports add a row each when their items land; the
/// `docs-affected` row is FEAT-086's and the `impact` row is FEAT-028's.
pub const OPS: &[Op] = &[
    op!(
        "find",
        "Symbols whose name matches the text.",
        &[TEXT],
        Some("find"),
        Call::Project(find)
    ),
    op!(
        "refs",
        "What references a symbol, including through re-exports.",
        &[TEXT],
        Some("refs"),
        Call::Project(refs)
    ),
    op!(
        "callers",
        "Callers of a symbol, up to depth hops.",
        &[TEXT, dflt("depth", Ty::Int(1), "1", "call hops to follow")],
        Some("callers"),
        Call::Project(callers)
    ),
    op!(
        "tests",
        "Tests that exercise or verify a symbol.",
        &[
            TEXT,
            dflt("depth", Ty::Int(0), "3", "call hops to follow"),
            opt("cases", Ty::Bool, "only test cases"),
        ],
        Some("tests"),
        Call::Project(tests)
    ),
    op!(
        "why",
        "The symbol at FILE:LINE and the rationale linked to it.",
        &[req(
            "location",
            "FILE:LINE, FILE relative to the repository root"
        )],
        Some("why"),
        Call::Project(why)
    ),
    op!(
        "explain",
        "A symbol's signature, doc, links and rationale.",
        &[
            req("symbol", "canonical name or plain name"),
            dflt("per-side", Ty::Int(0), "10", "links listed on each side"),
        ],
        Some("explain"),
        Call::Project(explain)
    ),
    op!(
        "show",
        "A symbol's source lines, or only its signature and doc.",
        &[
            req("symbol", "canonical name or plain name"),
            opt("signature", Ty::Bool, "only the signature and doc"),
        ],
        Some("show"),
        Call::Project(show)
    ),
    op!(
        "path",
        "Shortest chain of links between two symbols.",
        &[
            req("from", "start symbol"),
            req("to", "end symbol"),
            dflt("max-len", Ty::Int(1), "6", "longest chain to look for"),
        ],
        Some("path"),
        Call::Project(path)
    ),
    op!(
        "map",
        "Project outline: symbols ranked by PageRank.",
        &[],
        Some("map"),
        Call::Project(map)
    ),
    op!(
        "search",
        "Symbols ranked by BM25 over names, docs, sections and contract keys.",
        &[req("terms", "words to look for"), LIMIT20],
        Some("search"),
        Call::Project(search)
    ),
    op!(
        "dangling",
        "References that name something that does not exist.",
        &[opt(
            "kind",
            Ty::Str,
            "link kinds, comma separated, or `contract`"
        )],
        Some("dangling"),
        Call::Project(dangling)
    ),
    op!(
        "deps",
        "Package versions the lockfiles pin, the members that pin them and the code that uses them.",
        &[
            opt("package", Ty::Str, "only this package name"),
            dflt("limit", Ty::Int(1), "1000", "how many versions to list"),
            dflt(
                "used-by-limit",
                Ty::Int(1),
                "20",
                "how many users to list per version"
            ),
        ],
        Some("deps"),
        Call::Project(deps)
    ),
    op!(
        "mirrors",
        "Mirrored copies of a structure that differ.",
        &[opt("kind", Ty::Str, "contract kinds, comma separated")],
        Some("mirrors"),
        Call::Project(mirrors)
    ),
    op!(
        "docs-affected",
        "Documents that link to code changed since a revision or position.",
        &[
            opt("since", Ty::Str, "a revision or a clean position"),
            opt("kind", Ty::Str, "link kinds, comma separated"),
            dflt("limit", Ty::Int(1), "200", "how many documents to report"),
        ],
        Some("docs-affected"),
        Call::Project(docs_affected)
    ),
    op!(
        "impact",
        "What a symbol, file, change range or item reaches, and the tests over it.",
        &[
            opt(
                "text",
                Ty::Str,
                "symbol or contract, canonical or plain name"
            ),
            opt(
                "file",
                Ty::Str,
                "`<repo>:<path>`, or a path in the marked repository"
            ),
            opt("range", Ty::Str, "change range `BASE..HEAD`"),
            opt(
                "since",
                Ty::Str,
                "changes since a revision or clean position"
            ),
            opt("item", Ty::Str, "board item code"),
            dflt("depth", Ty::Int(0), "3", "call hops to follow"),
            dflt(
                "limit",
                Ty::Int(1),
                "500",
                "most nodes in the groups, and most tests"
            ),
            dflt(
                "test-depth",
                Ty::Int(0),
                "3",
                "hops the tests section follows"
            ),
        ],
        Some("impact"),
        Call::Project(impact)
    ),
    op!(
        "lineage",
        "What a type, schema element, symbol or contract is and where it goes.",
        &[
            TEXT,
            dflt("depth", Ty::Int(1), "3", "contract hops to follow"),
            dflt("limit", Ty::Int(1), "200", "most hits in all groups"),
            dflt("per-group", Ty::Int(1), "10", "most hits listed per group"),
        ],
        Some("lineage"),
        Call::Project(lineage)
    ),
    op!(
        "status",
        "Index watermark, pending changes and staleness.",
        &[],
        None,
        Call::Project(status)
    ),
    op!(
        "position",
        "The checkout's position and what the index reflects.",
        &[opt("repo", Ty::Str, "the repo to show")],
        Some("position"),
        Call::Free(position)
    ),
    op!(
        "hubs",
        "The nodes with the highest weighted degree.",
        &[
            LEVEL_SYMBOL,
            opt("min-degree", Ty::Float, "smallest weighted degree listed"),
            dflt("limit", Ty::Int(1), "20", "how many hubs to list"),
            STRICT,
        ],
        Some("hubs"),
        Call::Project(hubs)
    ),
    op!(
        "cycles",
        "Dependency cycles, each with one witness.",
        &[
            LEVEL_FILE,
            opt("include-inferred", Ty::Bool, "also follow inferred links"),
            dflt("members", Ty::Int(0), "10", "members listed per cycle"),
            dflt("limit", Ty::Int(1), "20", "how many cycles to list"),
        ],
        Some("cycles"),
        Call::Project(cycles)
    ),
    op!(
        "communities",
        "Groups of nodes that use each other more than the rest.",
        &[
            LEVEL_FILE,
            opt("seed", Ty::Int(0), "seed of the search"),
            opt("resolution", Ty::Float, "modularity resolution"),
            dflt("members", Ty::Int(0), "5", "members listed per community"),
            dflt("limit", Ty::Int(1), "20", "how many communities to list"),
            opt("of", Ty::Str, "only the community of this node"),
            STRICT,
        ],
        Some("communities"),
        Call::Project(communities)
    ),
    op!(
        "dead-code",
        "Code no entry point reaches.",
        &[
            opt(
                "internal",
                Ty::Bool,
                "do not count public-API roots as entry points"
            ),
            dflt(
                "min-confidence",
                Ty::Enum(&["high", "medium", "low"]),
                "medium",
                "least confidence listed"
            ),
            opt("path", Ty::Str, "only symbols under this path prefix"),
            dflt("limit", Ty::Int(1), "50", "how many candidates to list"),
            opt("check", Ty::Bool, "fail when any candidate is listed"),
        ],
        Some("dead-code"),
        Call::Project(dead_code)
    ),
    op!(
        "coverage",
        "How complete the index is.",
        &[
            opt("repo", Ty::Str, "only this repository"),
            dflt(
                "suffixes",
                Ty::Int(0),
                "20",
                "suffixes listed per repository"
            ),
        ],
        Some("coverage"),
        Call::Project(coverage)
    ),
    op!(
        "rationale",
        "The rationale of a symbol, a path or the top hubs.",
        &[
            opt("symbol", Ty::Str, "canonical name or plain name"),
            opt("path", Ty::Str, "only files under this path prefix"),
            opt("hubs", Ty::Int(0), "the top N hubs"),
            dflt("limit", Ty::Int(1), "100", "how many entries to list"),
        ],
        Some("rationale"),
        Call::Project(rationale)
    ),
    op!(
        "doctor",
        "Check the index and name a fix for each problem.",
        &[
            dflt(
                "fail-on",
                Ty::Enum(&["error", "warning"]),
                "warning",
                "lowest severity that fails"
            ),
            opt(
                "skip-git",
                Ty::Bool,
                "do not compare shards with the kindex git repository"
            ),
            opt(
                "verbose",
                Ty::Bool,
                "list repo-relative keys for each finding"
            ),
        ],
        Some("doctor"),
        Call::Free(doctor)
    ),
    op!(
        "context-list",
        "The base and every context on this machine.",
        &[],
        None,
        Call::Project(context_list)
    ),
    op!(
        "context-diff",
        "Compare two contexts' graphs.",
        &[
            req("a", "`base`, `current` or an id"),
            req("b", "`base`, `current` or an id"),
            dflt("limit", Ty::Int(1), "50", "entries per list"),
        ],
        None,
        Call::Project(context_diff)
    ),
    op!(
        "views/",
        "The view ops and their arguments.",
        &[],
        None,
        Call::Free(views_catalogue)
    ),
    op!(
        "views/structure",
        "A structure view at a position: Mermaid and the graph.",
        &[
            dflt(
                "level",
                Ty::Enum(VIEW_LEVELS),
                "package",
                "node granularity"
            ),
            MAX_NODES,
            dflt("inferred", Ty::Bool, "false", "also draw inferred links"),
        ],
        Some("view"),
        Call::Project(view_structure)
    ),
    op!(
        "views/contracts",
        "A contracts view at a position: Mermaid and the graph.",
        &[
            opt("kind", Ty::Str, "only contracts of this kind"),
            MAX_NODES
        ],
        Some("view"),
        Call::Project(view_contracts)
    ),
    op!(
        "views/callers",
        "A callers view at a position: Mermaid and the graph.",
        &[
            req("symbol", "canonical name or plain name"),
            dflt("depth", Ty::Int(1), "2", "call hops to follow"),
            MAX_NODES,
        ],
        Some("view"),
        Call::Project(view_callers)
    ),
    op!(
        "views/impact",
        "An impact view at a position: Mermaid and the graph.",
        &[
            req("symbol", "canonical name or plain name"),
            dflt("depth", Ty::Int(1), "3", "hops to follow"),
            MAX_NODES,
        ],
        Some("view"),
        Call::Project(view_impact)
    ),
    op!(
        "views/lineage",
        "A lineage view at a position: Mermaid and the graph.",
        &[
            req("symbol", "canonical name or plain name"),
            dflt("depth", Ty::Int(1), "3", "contract hops to follow"),
            MAX_NODES,
        ],
        Some("view"),
        Call::Project(view_lineage)
    ),
    op!(
        "views/path",
        "A path view at a position: Mermaid and the graph.",
        &[
            req("from", "start symbol"),
            req("to", "end symbol"),
            dflt("max-len", Ty::Int(1), "6", "longest chain to look for"),
        ],
        Some("view"),
        Call::Project(view_path)
    ),
    op!(
        "views/cycles",
        "A cycles view at a position: Mermaid and the graph.",
        &[
            dflt(
                "level",
                Ty::Enum(VIEW_LEVELS),
                "package",
                "node granularity"
            ),
            dflt("limit", Ty::Int(1), "5", "how many cycles to draw"),
        ],
        Some("view"),
        Call::Project(view_cycles)
    ),
    op!(
        "timeline",
        "The stored views and snapshot views, oldest first, for a time slider. Reads no index.",
        &[dflt(
            "limit",
            Ty::Int(1),
            "1000",
            "most points returned; the newest are kept"
        )],
        Some("timeline"),
        Call::Project(timeline)
    ),
    op!(
        "stored-view",
        "The stored bytes of a view by its view ID, as a view route answers them.",
        &[req("id", "a view ID, `v1-` and 32 hex digits")],
        Some("view"),
        Call::Project(stored_view)
    ),
    op!(
        "context-conflicts",
        "Contracts that both contexts changed against the base.",
        &[
            req("a", "`base`, `current` or an id"),
            req("b", "`base`, `current` or an id")
        ],
        None,
        Call::Project(context_conflicts)
    ),
];

// ---- FEAT-088: generated views ----

/// Parameters a view does not take, though every op accepts them: they would change the bytes
/// of a stored answer.
const NOT_FOR_VIEWS: &[&str] = &["budget", "diagnostic", "expand"];

const VIEW_LEVELS: &[&str] = &["file", "package"];
const MAX_NODES: Param = dflt("max-nodes", Ty::Int(1), "60", "most nodes drawn");

/// The view question a request asks: the op's arguments, by name, from the validated
/// parameters.
fn view_spec(op: &str, a: &Args) -> Result<katalogr_dist::render::ViewSpec, DistError> {
    if let Some(p) = NOT_FOR_VIEWS.iter().find(|p| a.opt(p).is_some()) {
        return Err(DistError::Usage(format!(
            "`{p}` is not a parameter of a view"
        )));
    }
    let info = katalogr_dist::render::catalogue()
        .into_iter()
        .find(|o| o.name == op)
        .ok_or_else(|| DistError::Usage(format!("unknown view op `{op}`")))?;
    let given: BTreeMap<String, String> = info
        .args
        .iter()
        .filter_map(|arg| {
            a.opt(arg.name)
                .map(|v| (arg.name.to_string(), v.to_string()))
        })
        .collect();
    katalogr_dist::render::ViewSpec::from_args(op, &given)
        .map_err(|e| DistError::Usage(e.to_string()))
}

fn view_of(op: &str, k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
    match view_spec(op, a) {
        Ok(spec) => answer(k.view(&spec, o), render_json::view_answer),
        Err(e) => render_json::error(&e),
    }
}

macro_rules! view_fn {
    ($name:ident, $op:expr) => {
        fn $name(k: &Katalogr, a: &Args, o: &Options) -> JsonAnswer {
            view_of($op, k, a, o)
        }
    };
}
view_fn!(view_structure, "structure");
view_fn!(view_contracts, "contracts");
view_fn!(view_callers, "callers");
view_fn!(view_impact, "impact");
view_fn!(view_lineage, "lineage");
view_fn!(view_path, "path");
view_fn!(view_cycles, "cycles");

/// `GET /api/v1/timeline`: one facade call, no query and no wait.
fn timeline(k: &Katalogr, a: &Args, _: &Options) -> JsonAnswer {
    answer(k.timeline(a.int("limit") as usize), render_json::timeline)
}

/// `GET /api/v1/stored-view`: the stored bytes of one view ID.
fn stored_view(k: &Katalogr, a: &Args, _: &Options) -> JsonAnswer {
    answer(k.stored_view(a.str("id")), render_json::view_answer)
}

/// `GET /api/v1/views/`: the ops and their arguments, sorted by op.
fn views_catalogue(_: &Path, _: &Overrides, a: &Args, _: &Options) -> JsonAnswer {
    if let Some(p) = NOT_FOR_VIEWS.iter().find(|p| a.opt(p).is_some()) {
        return render_json::error(&DistError::Usage(format!(
            "`{p}` is not a parameter of a view"
        )));
    }
    let views: Vec<serde_json::Value> = katalogr_dist::render::catalogue()
        .iter()
        .map(|op| {
            let params: Vec<serde_json::Value> = op
                .args
                .iter()
                .map(|p| {
                    let mut v = serde_json::json!({
                        "name": p.name,
                        "type": p.ty,
                        "required": p.required,
                    });
                    if let Some(d) = p.default {
                        v["default"] = d.into();
                    }
                    if !p.values.is_empty() {
                        v["values"] = p.values.into();
                    }
                    v
                })
                .collect();
            serde_json::json!({"op": op.name, "params": params})
        })
        .collect();
    let body = serde_json::json!({
        "schema": katalogr_dist::query::RESULT_SCHEMA_VERSION,
        "pele": katalogr_dist::render::PELE_VERSION,
        "views": views,
    });
    JsonAnswer {
        stdout: Some(body.to_string()),
        ..ok_answer()
    }
}

/// The view ID a request would get right now, with no query (`view_preflight`), for a views
/// route; `None` for any other route or when it cannot be known.
pub fn view_id_now(start: &Path, op: &Op, args: &Args, overrides: &Overrides) -> Option<String> {
    let name = op.name.strip_prefix("views/").filter(|n| !n.is_empty())?;
    let spec = view_spec(name, args).ok()?;
    let k = Katalogr::resolve(start, overrides).ok()?;
    let o = Options {
        consistency: Some(katalogr_dist::Consistency::StaleOk),
        timeout: args.float("timeout"),
        at: args.opt("at").map(str::to_string),
        ..Options::default()
    };
    k.view_preflight(&spec, &o).ok().flatten().map(|(id, _)| id)
}

/// The route of `name` in `ops`.
pub fn find_op<'a>(ops: &'a [Op], name: &str) -> Option<&'a Op> {
    ops.iter().find(|o| o.name == name)
}

/// One facade call for `op`: the project is resolved for it, or the op resolves for itself.
pub fn call_op(
    start: &Path,
    op: &Op,
    args: &Args,
    overrides: &Overrides,
    o: &Options,
) -> JsonAnswer {
    match op.call {
        Call::Project(f) => match Katalogr::resolve(start, overrides) {
            Ok(k) => f(&k, args, o),
            Err(e) => render_json::error(&e),
        },
        Call::Free(f) => f(start, overrides, args, o),
    }
}

/// One row by name, exactly as a request would run it, with no socket and no gate: the same
/// validation, the same base options and the same renderer. An unknown name or a bad parameter
/// is `DistError::Usage`.
pub fn call_by_name(
    start: &Path,
    overrides: &Overrides,
    name: &str,
    params: &BTreeMap<String, String>,
) -> Result<JsonAnswer, DistError> {
    let op = find_op(OPS, name).ok_or_else(|| DistError::Usage(format!("no such op: {name}")))?;
    let args = Args::parse(op.params, params).map_err(|e| DistError::Usage(e.message))?;
    let mut overrides = overrides.clone();
    overrides.context = args.opt("context").map(str::to_string);
    let base = Options {
        consistency: Some(args.consistency()),
        timeout: args.float("timeout"),
        diagnostic: false,
        expand: false,
        at: args.opt("at").map(str::to_string),
        view: None,
        ranking: katalogr_dist::Ranking::Neutral,
        fresh: false,
    };
    Ok(call_op(start, op, &args, &overrides, &base))
}
