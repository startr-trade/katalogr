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
        Some(Command::Notify { deleted, paths }) => cmd::notify::run(cli, *deleted, paths),
        Some(Command::Index { since, range, full }) => {
            cmd::index::run(cli, since.as_ref(), range.as_ref(), *full)
        }
        Some(Command::Find { text }) => cmd::find::run(cli, text),
        Some(Command::Refs { text }) => cmd::refs::run(cli, text),
        Some(Command::Callers { text, depth }) => cmd::callers::run(cli, text, *depth),
        Some(Command::Why { location }) => cmd::why::run(cli, location),
        Some(Command::Map) => cmd::map::run(cli),
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
