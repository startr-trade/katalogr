//! The katalogr CLI: a thin wrapper over katalogr-dist (ADR-0010).

mod cli;
mod cmd;
mod exit;
mod render;

use clap::{CommandFactory, Parser};
use katalogr_dist::DistError;

use crate::cli::{Cli, Command};
use crate::exit::ExitCode;

fn run(cli: &Cli) -> Result<ExitCode, DistError> {
    match &cli.command {
        Some(Command::Kindex { action }) => cmd::kindex::run(action),
        Some(Command::Project { action }) => cmd::project::run(cli, action),
        Some(Command::Notify {
            deleted,
            from_git,
            paths,
        }) => cmd::notify::run(cli, *deleted, from_git.as_deref(), paths),
        Some(Command::Hook { action }) => cmd::hook::run(cli, action),
        Some(Command::Hooks { action }) => cmd::hooks::run(cli, action),
        Some(Command::Claude { action }) => cmd::claude::run(cli, action),
        Some(Command::Skill { action }) => cmd::skill::run(cli, action),
        Some(Command::SessionStart) => cmd::session::run(cli),
        Some(Command::Index {
            since,
            range,
            full,
            history,
        }) => cmd::index::run(cli, since.as_ref(), range.as_ref(), *full, *history),
        Some(Command::Doctor {
            fail_on,
            skip_git,
            verbose,
        }) => cmd::doctor::run(cli, *fail_on, *skip_git, *verbose),
        Some(Command::Eval { action }) => cmd::eval::run(cli, action),
        Some(Command::Dangling { kind }) => cmd::dangling::run(cli, kind),
        Some(Command::Mirrors { kind }) => cmd::mirrors::run(cli, kind),
        Some(Command::Hubs {
            level,
            min_degree,
            limit,
            strict,
        }) => cmd::hubs::run(cli, *level, *min_degree, *limit, *strict),
        Some(Command::Coverage { repo, suffixes }) => {
            cmd::coverage::run(cli, repo.as_deref(), *suffixes)
        }
        Some(Command::Communities {
            level,
            seed,
            resolution,
            members,
            limit,
            of,
            strict,
        }) => cmd::communities::run(
            cli,
            *level,
            *seed,
            *resolution,
            *members,
            *limit,
            of.as_deref(),
            *strict,
        ),
        Some(Command::Cycles {
            level,
            include_inferred,
            members,
            limit,
        }) => cmd::cycles::run(cli, *level, *include_inferred, *members, *limit),
        Some(Command::DeadCode {
            internal,
            min_confidence,
            path,
            limit,
            check,
        }) => cmd::dead_code::run(
            cli,
            *internal,
            *min_confidence,
            path.as_deref(),
            *limit,
            *check,
        ),
        Some(Command::Rationale {
            symbol,
            path,
            hubs,
            limit,
        }) => cmd::rationale::run(cli, symbol.as_deref(), path.as_deref(), *hubs, *limit),
        Some(Command::Find { text }) => cmd::find::run(cli, text),
        Some(Command::Refs { text, diagnostic }) => cmd::refs::run(cli, text, *diagnostic),
        Some(Command::Callers { text, depth }) => cmd::callers::run(cli, text, *depth),
        Some(Command::Why { location }) => cmd::why::run(cli, location),
        Some(Command::Map { diagnostic }) => cmd::map::run(cli, *diagnostic),
        Some(Command::Search {
            terms,
            limit,
            diagnostic,
        }) => cmd::search::run(cli, terms, *limit, *diagnostic),
        Some(Command::Show { symbol, signature }) => cmd::show::run(cli, symbol, *signature),
        Some(Command::Path { from, to, max_len }) => cmd::path::run(cli, from, to, *max_len),
        Some(Command::Explain { symbol, per_side }) => cmd::explain::run(cli, symbol, *per_side),
        Some(Command::Replace { symbol, file }) => cmd::replace::run(cli, symbol, file),
        None => {
            // `--version` and `--help` are handled by clap and exit 0 before this.
            Cli::command().print_help().expect("write help to stdout");
            Ok(ExitCode::Usage)
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let code = run(&cli).unwrap_or_else(|e| ExitCode::from_error(&e));
    std::process::exit(code.code());
}
