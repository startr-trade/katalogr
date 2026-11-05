//! The katalogr CLI: a thin wrapper over katalogr-dist (ADR-0010).

mod answer_build;
#[cfg(feature = "api")]
mod api;
mod cli;
mod cmd;
mod exit;
#[cfg(feature = "mcp")]
mod mcp;
// Unit tests of the MCP wrapper, as crate-level modules named after what they test.
mod render;
// Unit tests of the API wrapper, likewise.
#[cfg(all(test, feature = "api"))]
#[path = "api/guard_tests.rs"]
mod api_guard;
#[cfg(all(test, feature = "api"))]
#[path = "api/http_tests.rs"]
mod api_http;
#[cfg(all(test, feature = "mcp"))]
#[path = "mcp/tools_tests.rs"]
mod tools;
#[cfg(all(test, feature = "mcp"))]
#[path = "mcp/transport_tests.rs"]
mod transport;

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
        Some(Command::Status) => cmd::status::run(cli),
        Some(Command::Contract) => cmd::contract::run(cli),
        Some(Command::Watch { interval }) => cmd::watch::run(cli, *interval),
        Some(Command::Position { repo }) => cmd::position::run(cli, repo.as_deref()),
        #[cfg(feature = "api")]
        Some(Command::Api {
            listen,
            workers,
            queue,
            no_process,
            cors_origin,
        }) => cmd::api::run(
            cli,
            *listen,
            *workers,
            *queue,
            *no_process,
            cors_origin.clone(),
        ),
        #[cfg(feature = "mcp")]
        Some(Command::Mcp {
            root,
            output_schemas,
            progress_ms,
        }) => cmd::mcp::run(cli, root.as_deref(), *output_schemas, *progress_ms),
        Some(Command::Doctor {
            fail_on,
            skip_git,
            verbose,
        }) => cmd::doctor::run(cli, *fail_on, *skip_git, *verbose),
        Some(Command::Eval { action }) => cmd::eval::run(cli, action),
        Some(Command::Context { action }) => cmd::context::run(cli, action),
        Some(Command::Profiles { action }) => cmd::profiles::run(cli, action),
        Some(Command::Dangling { kind }) => cmd::dangling::run(cli, kind),
        Some(Command::DocsAffected { since, kind, limit }) => {
            cmd::docs_affected::run(cli, since.as_deref(), kind, *limit)
        }
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
        Some(Command::Refs {
            text,
            diagnostic,
            expand,
        }) => cmd::refs::run(cli, text, *diagnostic, *expand),
        Some(Command::Callers {
            text,
            depth,
            expand,
        }) => cmd::callers::run(cli, text, *depth, *expand),
        Some(Command::Tests { text, depth, cases }) => cmd::tests::run(cli, text, *depth, *cases),
        Some(Command::Why { location, no_use }) => cmd::why::run(cli, location, *no_use),
        Some(Command::Map {
            diagnostic,
            neutral,
        }) => cmd::map::run(cli, *diagnostic, *neutral),
        Some(Command::Search {
            terms,
            limit,
            diagnostic,
            neutral,
        }) => cmd::search::run(cli, terms, *limit, *diagnostic, *neutral),
        Some(Command::Show { symbol, signature }) => cmd::show::run(cli, symbol, *signature),
        Some(Command::Path { from, to, max_len }) => cmd::path::run(cli, from, to, *max_len),
        Some(Command::Render {
            action,
            format,
            words,
        }) => cmd::render::run(cli, action.as_ref(), *format, words),
        Some(Command::View {
            action,
            fresh,
            words,
        }) => cmd::view::run(cli, action.as_ref(), *fresh, words),
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
    let cli = Cli::parse_from(cmd::render::hoist_globals(std::env::args_os().collect()));
    let code = run(&cli).unwrap_or_else(|e| ExitCode::from_error(&e));
    std::process::exit(code.code());
}
