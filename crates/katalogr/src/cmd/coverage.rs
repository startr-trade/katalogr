//! `katalogr coverage [--repo NAME] [--suffixes N]`.

use katalogr_dist::{CoverageOptions, DistError};

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, repo: Option<&str>, suffixes: usize) -> Result<ExitCode, DistError> {
    let opts = CoverageOptions {
        repo: repo.map(str::to_string),
        suffix_top: suffixes,
    };
    let reply = resolve(cli)?.coverage(
        &opts,
        &options_for(cli, "coverage", &[("options", format!("{opts:?}"))]),
    )?;
    Ok(render::insight(cli, reply, "coverage", &render::COVERAGE))
}
