//! The katalogr CLI: a thin wrapper over katalogr-dist (ADR-0010).

mod cli;
mod cmd;
mod exit;

use clap::{CommandFactory, Parser};

use crate::exit::ExitCode;

fn main() {
    // No subcommands exist yet, so every invocation that reaches here is
    // "no subcommand": print help and report a usage error (exit 2).
    // `--version` and `--help` are handled by clap and exit 0 before this.
    let _cli = cli::Cli::parse();
    cli::Cli::command()
        .print_help()
        .expect("write help to stdout");
    std::process::exit(ExitCode::Usage.code());
}
