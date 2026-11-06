//! `katalogr export html|graph` (FEAT-034): static files for reviewers.
//!
//! `html` runs a plan of calls through the API's own op table, with no socket, and writes one
//! self-contained file holding the page and the answers. It is all or nothing: everything is
//! held in memory until the end, and the write is a rename. `graph` writes one view as DOT,
//! GraphML or Cytoscape JSON.

use std::path::Path;

use katalogr_dist::render::{ViewSpec, to_cytoscape_json, to_dot, to_graphml};
use katalogr_dist::{DistError, Outcome};

use crate::cli::{Cli, ExportAction, ExportGraphArgs, GraphFormat};
use crate::cmd::render::{graph_reply, line_of};
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, action: &ExportAction) -> Result<ExitCode, DistError> {
    match action {
        #[cfg(feature = "web")]
        ExportAction::Html(args) => html(cli, args),
        ExportAction::Graph(args) => graph(cli, args),
    }
}

/// A temp file in the target's folder, then a rename. An existing file is replaced, and a
/// failure leaves neither a temp file nor a changed target.
fn write_atomic(path: &Path, text: &str) -> Result<(), DistError> {
    let name = path
        .file_name()
        .ok_or_else(|| DistError::Usage(format!("{} is not a file name", path.display())))?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let done = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&tmp);
        return Err(DistError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        });
    }
    Ok(())
}

/// `--format` and `--out` among the view's words: the words after the op are taken whole, so
/// the flags are picked out here. The last one wins.
fn take_flag(words: &mut Vec<String>, flag: &str) -> Result<Option<String>, DistError> {
    let mut found = None;
    let mut rest = Vec::with_capacity(words.len());
    let mut it = std::mem::take(words).into_iter();
    while let Some(w) = it.next() {
        if w == flag {
            found = Some(
                it.next()
                    .ok_or_else(|| DistError::Usage(format!("{flag} wants a value")))?,
            );
        } else if let Some(v) = w.strip_prefix(&format!("{flag}=")) {
            found = Some(v.to_string());
        } else {
            rest.push(w);
        }
    }
    *words = rest;
    Ok(found)
}

pub fn graph(cli: &Cli, args: &ExportGraphArgs) -> Result<ExitCode, DistError> {
    use clap::ValueEnum;
    let mut words = args.words.clone();
    let format = match (args.format, take_flag(&mut words, "--format")?) {
        (_, Some(v)) => GraphFormat::from_str(&v, true).map_err(|_| {
            DistError::Usage(format!("unknown --format `{v}`: use dot, graphml or json"))
        })?,
        (Some(f), None) => f,
        (None, None) => {
            return Err(DistError::Usage(
                "export graph wants --format dot, graphml or json".to_string(),
            ));
        }
    };
    let out = match (&args.out, take_flag(&mut words, "--out")?) {
        (_, Some(v)) => std::path::PathBuf::from(v),
        (Some(p), None) => p.clone(),
        (None, None) => {
            return Err(DistError::Usage(
                "export graph wants --out FILE".to_string(),
            ));
        }
    };
    if words.is_empty() {
        return Err(DistError::Usage(
            "export graph needs a view: structure, contracts, callers <symbol>, path <from> <to> or cycles"
                .to_string(),
        ));
    }
    let spec = ViewSpec::parse(&line_of(&words)).map_err(|e| DistError::Usage(e.to_string()))?;
    let reply = graph_reply(&spec, resolve(cli)?.view(&spec, &options(cli))?)?;
    if !matches!(reply.outcome, Outcome::Found(_)) {
        // Nothing to write: report it as `render` does.
        return render::view(cli, reply, |_| Ok(String::new()));
    }
    let code = render::json::exit_for(&reply);
    render::note_stale(&reply.report);
    for p in &reply.partial {
        eprintln!("{}", p.line());
    }
    let Outcome::Found(g) = reply.outcome else {
        unreachable!("checked above");
    };
    if g.total > g.shown {
        eprintln!(
            "katalogr: warning: the view shows {} of {} nodes",
            g.shown, g.total
        );
    }
    let mut text = match format {
        GraphFormat::Dot => to_dot(&g),
        GraphFormat::Graphml => to_graphml(&g),
        GraphFormat::Json => to_cytoscape_json(&g),
    };
    if !text.ends_with('\n') {
        text.push('\n');
    }
    write_atomic(&out, &text)?;
    eprintln!("katalogr: export: wrote {} bytes", text.len());
    Ok(code)
}

#[cfg(feature = "web")]
pub use html_export::html;

#[cfg(feature = "web")]
mod html_export {
    use std::collections::{BTreeMap, BTreeSet};

    use katalogr_dist::web::export::{
        EmbeddedAnswer, ExportData, PlanOptions, PlannedCall, PositionRef, answer_key, plan_symbol,
        plan_views, positions_agree, single_file, strip_volatile,
    };
    use katalogr_dist::{DistError, Overrides};
    use serde_json::{Value, json};

    use super::write_atomic;
    use crate::api::http::{error_body, redact, status_of};
    use crate::api::ops::call_by_name;
    use crate::cli::{Cli, ExportHtmlArgs};
    use crate::cmd::{overrides, resolve};
    use crate::exit::ExitCode;

    fn refuse(exit: i32, message: String) -> DistError {
        DistError::Refused { exit, message }
    }

    /// Everything one run keeps.
    struct Collector {
        cwd: std::path::PathBuf,
        overrides: Overrides,
        /// `consistency`, `timeout` and `at`, set on every call.
        common: BTreeMap<String, String>,
        roots: Vec<String>,
        seen: Vec<Vec<PositionRef>>,
        repos: BTreeSet<String>,
        answers: BTreeMap<String, EmbeddedAnswer>,
    }

    /// Repo names: `repo` and `group` values anywhere in the body.
    fn repo_names(v: &Value, out: &mut BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    match (k.as_str(), x) {
                        ("repo" | "group", Value::String(s)) if !s.is_empty() => {
                            out.insert(s.clone());
                        }
                        _ => repo_names(x, out),
                    }
                }
            }
            Value::Array(a) => a.iter().for_each(|x| repo_names(x, out)),
            _ => {}
        }
    }

    fn holds_root(v: &Value, roots: &[String]) -> bool {
        match v {
            Value::String(s) => roots
                .iter()
                .any(|r| !r.is_empty() && s.contains(r.as_str())),
            Value::Array(a) => a.iter().any(|x| holds_root(x, roots)),
            Value::Object(m) => m.values().any(|x| holds_root(x, roots)),
            _ => false,
        }
    }

    impl Collector {
        /// Runs one call and keeps its answer. Stops the run, writing nothing, on an answer
        /// that is not exact, a position that is unavailable, an error, or a path.
        fn run(&mut self, call: &PlannedCall) -> Result<Value, DistError> {
            let key = answer_key(call);
            if let Some(done) = self.answers.get(&key) {
                return Ok(done.body.clone());
            }
            let mut params = call.params.clone();
            for (k, v) in &self.common {
                params.entry(k.clone()).or_insert_with(|| v.clone());
            }
            let ans = call_by_name(&self.cwd, &self.overrides, &call.op, &params)?;
            let what = &call.op;
            match ans.exit {
                ExitCode::PositionUnavailable if ans.stdout.is_none() => {
                    return Err(refuse(
                        6,
                        format!("export stopped: the position is unavailable ({what})"),
                    ));
                }
                ExitCode::Stale => {
                    return Err(refuse(
                        5,
                        format!("export stopped: the answer to {what} is stale"),
                    ));
                }
                ExitCode::Error | ExitCode::Usage if ans.stdout.is_none() => {
                    return Err(refuse(
                        1,
                        format!(
                            "export stopped: {what} failed: {}",
                            redact(ans.message.as_deref().unwrap_or("no message"))
                        ),
                    ));
                }
                _ => {}
            }
            let status = if ans.stdout.is_some() && ans.exit == ExitCode::Error {
                200
            } else {
                status_of(ans.exit)
            };
            let mut body: Value = match &ans.stdout {
                Some(text) => serde_json::from_str(text).map_err(|e| {
                    refuse(1, format!("export stopped: {what} answered badly: {e}"))
                })?,
                None => {
                    let kind = if ans.exit == ExitCode::NoMatch {
                        "no-match"
                    } else {
                        "error"
                    };
                    serde_json::from_str(&error_body(
                        kind,
                        &redact(ans.message.as_deref().unwrap_or("")),
                    ))
                    .expect("an error body is JSON")
                }
            };
            if ans.exit == ExitCode::PositionUnavailable {
                return Err(refuse(
                    6,
                    format!("export stopped: the position is unavailable ({what})"),
                ));
            }
            if body.get("exact") == Some(&Value::Bool(false)) {
                return Err(refuse(
                    5,
                    format!("export stopped: the answer to {what} is not exact"),
                ));
            }
            strip_volatile(&mut body);
            if holds_root(&body, &self.roots) {
                return Err(refuse(
                    1,
                    format!("export stopped: the answer to {what} holds an absolute path"),
                ));
            }
            if let Some(p) = body.get("positions")
                && let Ok(list) = serde_json::from_value::<Vec<PositionRef>>(p.clone())
            {
                for pos in &list {
                    self.repos.insert(pos.repo.clone());
                }
                self.seen.push(list);
            }
            repo_names(&body, &mut self.repos);
            self.answers.insert(
                key,
                EmbeddedAnswer {
                    status,
                    body: body.clone(),
                },
            );
            Ok(body)
        }
    }

    fn call(op: &str, params: &[(&str, String)]) -> PlannedCall {
        PlannedCall {
            op: op.to_string(),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    pub fn html(cli: &Cli, args: &ExportHtmlArgs) -> Result<ExitCode, DistError> {
        let k = resolve(cli)?;
        let cwd = std::env::current_dir()
            .map_err(|e| DistError::Usage(format!("current directory: {e}")))?;
        let mut common = BTreeMap::new();
        // `wait`, unless the person asked for another mode (a `stale-ok` run is then refused at
        // the first answer that is not exact).
        let mode = match cli.consistency {
            Some(crate::cli::Consistency::WaitAll) => "wait-all",
            Some(crate::cli::Consistency::StaleOk) => "stale-ok",
            _ => "wait",
        };
        common.insert("consistency".to_string(), mode.to_string());
        if let Some(t) = cli.timeout {
            common.insert("timeout".to_string(), t.to_string());
        }
        if let Some(at) = &cli.at {
            common.insert("at".to_string(), at.clone());
        }
        let info = katalogr_dist::contract::contract_info();
        // The file serves no pages, so `web-view` is not among its capabilities.
        let capabilities: Vec<String> = info
            .capabilities
            .into_iter()
            .filter(|c| c != "web-view")
            .collect();
        let mut c = Collector {
            cwd,
            overrides: overrides(cli),
            common,
            roots: k.root_paths(),
            seen: Vec::new(),
            repos: BTreeSet::new(),
            answers: BTreeMap::new(),
        };
        for planned in plan_views(&capabilities) {
            c.run(&planned)?;
        }
        let hubs = c.run(&call(
            "hubs",
            &[
                ("level", "symbol".to_string()),
                ("limit", args.max_symbols.to_string()),
                // The N most connected, whatever the project's hub threshold: a small project
                // has no symbol above it, and its file would list none.
                ("min-degree", "0".to_string()),
            ],
        ))?;
        let nodes: Vec<Value> = hubs["result"]["items"]
            .as_array()
            .map(|items| items.iter().map(|i| i["node"].clone()).collect())
            .unwrap_or_default();
        let mut partial = Vec::new();
        let total = hubs["result"]["nodes"].as_u64().unwrap_or(0);
        // Cut only when the cap was reached: a list shorter than the cap is the whole list.
        if nodes.len() >= args.max_symbols && total > nodes.len() as u64 {
            partial.push(json!({
                "at": "export.symbols",
                "cap": "limit",
                "limit": args.max_symbols,
                "shown": nodes.len(),
                "total": total,
                "note": "the symbols with the most links are kept"
            }));
        }
        let opts = PlanOptions {
            with_source: args.with_source,
        };
        let mut symbols = Vec::new();
        for node in nodes {
            let Some(name) = node["name"].as_str() else {
                continue;
            };
            let explain = c.run(&call("explain", &[("symbol", name.to_string())]))?;
            let sym = &explain["result"]["symbol"];
            let path = sym["path"].as_str().or(node["path"].as_str()).unwrap_or("");
            let line = sym["span"]["start_line"]
                .as_u64()
                .or(node["span"]["start_line"].as_u64())
                .unwrap_or(1) as u32;
            for planned in plan_symbol(name, path, line, &opts) {
                c.run(&planned)?;
            }
            symbols.push(node);
        }
        if !positions_agree(&c.seen) {
            return Err(refuse(
                5,
                "export stopped: the index moved during the run".to_string(),
            ));
        }
        let mut positions = c.seen.first().cloned().unwrap_or_default();
        positions.sort();
        positions.dedup();
        let data = ExportData {
            format: 1,
            katalogr: info.katalogr,
            with_source: args.with_source,
            capabilities,
            positions,
            repos: c.repos.iter().cloned().collect(),
            symbols,
            partial,
            answers: c.answers,
        };
        let text = single_file(&data);
        write_atomic(&args.out, &text)?;
        eprintln!("katalogr: export: wrote {} bytes", text.len());
        eprintln!("katalogr: export: repos named: {}", data.repos.join(", "));
        for p in &data.partial {
            eprintln!(
                "katalogr: export: holds the {} most connected of {} symbols (--max-symbols)",
                p["shown"], p["total"]
            );
        }
        Ok(ExitCode::Ok)
    }
}
