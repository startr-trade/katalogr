//! `katalogr session-start [--budget N]`: never fails.

use katalogr_dist::{DistError, Overrides};

use crate::cli::Cli;
use crate::exit::ExitCode;

pub fn run(cli: &Cli) -> Result<ExitCode, DistError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let overrides = Overrides {
        kindex: cli.kindex.clone(),
        project: cli.project.clone(),
    };
    print!(
        "{}",
        katalogr_dist::session_start(&cwd, &overrides, cli.budget, cli.timeout)
    );
    Ok(ExitCode::Ok)
}
