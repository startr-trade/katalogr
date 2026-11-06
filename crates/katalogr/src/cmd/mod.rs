//! Subcommand implementations. Each command is a thin wrapper over katalogr-dist.

pub mod answers;
#[cfg(feature = "api")]
pub mod api;
pub mod audit;
pub mod callers;
pub mod claude;
pub mod communities;
pub mod context;
pub mod contract;
pub mod coverage;
pub mod cycles;
pub mod dangling;
pub mod dead_code;
pub mod deps;
pub mod diff;
pub mod docs_affected;
pub mod doctor;
pub mod eval;
pub mod explain;
pub mod find;
pub mod hook;
pub mod hooks;
pub mod hubs;
pub mod impact;
pub mod index;
pub mod kindex;
pub mod map;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod mirrors;
pub mod notify;
pub mod path;
pub mod position;
pub mod profiles;
pub mod project;
pub mod rationale;
pub mod refs;
pub mod render;
pub mod render_doc;
pub mod replace;
pub mod search;
#[cfg(feature = "web")]
pub mod serve;
pub mod session;
pub mod shard;
pub mod show;
pub mod skill;
pub mod snapshot;
pub mod status;
pub mod tests;
pub mod version;
pub mod view;
pub mod view_doc;
pub mod watch;
pub mod why;

use std::path::Path;

use katalogr_dist::{DistError, Katalogr, Options, Overrides};

use crate::cli::{Cli, Consistency};
use crate::exit::ExitCode;

/// Resolve the project from the global flags and the current directory.
pub fn resolve(cli: &Cli) -> Result<Katalogr, DistError> {
    let cwd =
        std::env::current_dir().map_err(|e| DistError::Usage(format!("current directory: {e}")))?;
    let mut k = Katalogr::resolve(
        &cwd,
        &Overrides {
            kindex: cli.kindex.clone(),
            project: cli.project.clone(),
            context: context_choice(cli),
            snapshot_dirs: Vec::new(),
        },
    )?;
    k.set_answer_build(crate::answer_build::current());
    Ok(k)
}

/// `--context`, else `KATALOGR_CONTEXT`.
pub fn context_choice(cli: &Cli) -> Option<String> {
    cli.context.clone().or_else(|| {
        std::env::var("KATALOGR_CONTEXT")
            .ok()
            .filter(|v| !v.is_empty())
    })
}

/// [`resolve`], also reading these snapshot folders (FEAT-033).
pub fn resolve_with_snapshots(
    cli: &Cli,
    snapshots: &[std::path::PathBuf],
) -> Result<Katalogr, DistError> {
    let cwd =
        std::env::current_dir().map_err(|e| DistError::Usage(format!("current directory: {e}")))?;
    let mut o = overrides(cli);
    o.snapshot_dirs = snapshots.to_vec();
    let mut k = Katalogr::resolve(&cwd, &o)?;
    k.set_answer_build(crate::answer_build::current());
    Ok(k)
}

pub fn overrides(cli: &Cli) -> Overrides {
    Overrides {
        kindex: cli.kindex.clone(),
        project: cli.project.clone(),
        context: context_choice(cli),
        snapshot_dirs: Vec::new(),
    }
}

/// The insight graph options for `level`, with the project's excluded link kinds.
pub fn graph_options(
    level: crate::cli::LevelArg,
    include_inferred: bool,
    settings: &katalogr_dist::InsightSettings,
) -> katalogr_dist::analytics::GraphOptions {
    use crate::cli::LevelArg;
    use katalogr_dist::analytics::Level;
    let mut g = katalogr_dist::analytics::GraphOptions::new(
        match level {
            LevelArg::Symbol => Level::Symbol,
            LevelArg::File => Level::File,
            LevelArg::Package => Level::Package,
        },
        include_inferred,
    );
    g.exclude_kinds = settings.exclude_kinds.clone();
    g
}

/// The ranking of `search` and `map`: the working set boost unless `--neutral` (FEAT-039).
pub fn ranking(neutral: bool) -> katalogr_dist::Ranking {
    if neutral {
        katalogr_dist::Ranking::Neutral
    } else {
        katalogr_dist::Ranking::Default
    }
}

pub fn options(cli: &Cli) -> Options {
    options_with(cli, false)
}

/// The read options, with a diagnostic trace when `diagnostic` (a per-command flag).
pub fn options_with(cli: &Cli, diagnostic: bool) -> Options {
    Options {
        consistency: cli.consistency.map(|c| match c {
            Consistency::Wait => katalogr_dist::Consistency::Wait,
            Consistency::WaitAll => katalogr_dist::Consistency::WaitAll,
            Consistency::StaleOk => katalogr_dist::Consistency::StaleOk,
        }),
        timeout: cli.timeout,
        diagnostic,
        expand: false,
        at: cli.at.clone(),
        view: None,
        ranking: katalogr_dist::Ranking::Neutral,
        fresh: cli.fresh,
    }
}

/// The read options of a question (FEAT-084): `op` and `args` name it for the view ID. `args`
/// holds what changes the answer, never `--json`, `--timeout` or `--consistency`; `--budget`
/// is added when set, since it cuts the answer.
pub fn options_for(cli: &Cli, op: &str, args: &[(&str, String)]) -> Options {
    options_for_diag(cli, op, args, false)
}

/// [`options_for`] with a diagnostic trace when `diagnostic` (a per-command flag).
pub fn options_for_diag(cli: &Cli, op: &str, args: &[(&str, String)], diagnostic: bool) -> Options {
    let mut view = katalogr_dist::ViewQuery {
        op: op.to_string(),
        args: args
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    };
    if let Some(b) = cli.budget {
        view.args.insert("budget".to_string(), b.to_string());
    }
    Options {
        view: Some(view),
        ..options_with(cli, diagnostic)
    }
}

/// Print a read report and map staleness to the exit status.
pub fn render(cli: &Cli, r: &katalogr_dist::Report, line: &str) -> ExitCode {
    if cli.json {
        return crate::render::emit(&crate::render::json::report(r));
    }
    for w in &r.warnings {
        eprintln!("katalogr: warning: {w}");
    }
    println!(
        "{line}; watermark {}, {} pending{}",
        r.watermark,
        r.pending,
        if r.stale { " (stale)" } else { "" }
    );
    if r.timed_out {
        ExitCode::Stale
    } else {
        ExitCode::Ok
    }
}

pub fn path(s: &str) -> &Path {
    Path::new(s)
}
