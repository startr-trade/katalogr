//! `katalogr doctor [--fail-on error|warning] [--skip-git] [--verbose]`.

use katalogr_dist::DistError;
use katalogr_dist::doctor::{DoctorOptions, Severity, doctor};

use crate::cli::{Cli, FailOn};
use crate::cmd::overrides;
use crate::exit::ExitCode;
use crate::render;

pub fn run(
    cli: &Cli,
    fail_on: FailOn,
    skip_git: bool,
    verbose: bool,
) -> Result<ExitCode, DistError> {
    let cwd =
        std::env::current_dir().map_err(|e| DistError::Usage(format!("current directory: {e}")))?;
    let opts = DoctorOptions {
        fail_on: match fail_on {
            FailOn::Error => Severity::Error,
            FailOn::Warning => Severity::Warning,
        },
        skip_git,
        verbose,
        ..DoctorOptions::default()
    };
    let report = doctor(&cwd, &overrides(cli), &opts)?;
    Ok(render::doctor(cli, &report))
}
