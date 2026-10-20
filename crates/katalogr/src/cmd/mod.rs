//! Subcommand implementations. Each command is a thin wrapper over katalogr-dist.

pub mod callers;
pub mod claude;
pub mod communities;
pub mod coverage;
pub mod cycles;
pub mod dangling;
pub mod dead_code;
pub mod explain;
pub mod find;
pub mod hook;
pub mod hooks;
pub mod hubs;
pub mod index;
pub mod kindex;
pub mod map;
pub mod mirrors;
pub mod notify;
pub mod path;
pub mod project;
pub mod rationale;
pub mod refs;
pub mod replace;
pub mod search;
pub mod session;
pub mod show;
pub mod skill;
pub mod version;
pub mod why;

use std::path::Path;

use katalogr_dist::{DistError, Katalogr, Options, Overrides};

use crate::cli::{Cli, Consistency};
use crate::exit::ExitCode;

/// Resolve the project from the global flags and the current directory.
pub fn resolve(cli: &Cli) -> Result<Katalogr, DistError> {
    let cwd =
        std::env::current_dir().map_err(|e| DistError::Usage(format!("current directory: {e}")))?;
    Katalogr::resolve(
        &cwd,
        &Overrides {
            kindex: cli.kindex.clone(),
            project: cli.project.clone(),
        },
    )
}

pub fn overrides(cli: &Cli) -> Overrides {
    Overrides {
        kindex: cli.kindex.clone(),
        project: cli.project.clone(),
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

pub fn options(cli: &Cli) -> Options {
    Options {
        consistency: cli.consistency.map(|c| match c {
            Consistency::Wait => katalogr_dist::Consistency::Wait,
            Consistency::WaitAll => katalogr_dist::Consistency::WaitAll,
            Consistency::StaleOk => katalogr_dist::Consistency::StaleOk,
        }),
        timeout: cli.timeout,
    }
}

/// Print a read report and map staleness to the exit status.
pub fn render(cli: &Cli, r: &katalogr_dist::Report, line: &str) -> ExitCode {
    for w in &r.warnings {
        eprintln!("katalogr: warning: {w}");
    }
    if cli.json {
        println!("{}", r.to_json());
    } else {
        println!(
            "{line}; watermark {}, {} pending{}",
            r.watermark,
            r.pending,
            if r.stale { " (stale)" } else { "" }
        );
    }
    if r.timed_out {
        ExitCode::Stale
    } else {
        ExitCode::Ok
    }
}

pub fn path(s: &str) -> &Path {
    Path::new(s)
}
