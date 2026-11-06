//! `katalogr deps [--package NAME] [--limit N] [--used-by-limit N]`.

use katalogr_dist::{DepsOptions, DistError};

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(
    cli: &Cli,
    package: Option<&str>,
    limit: usize,
    used_by_limit: usize,
) -> Result<ExitCode, DistError> {
    let opts = DepsOptions {
        package: package.map(str::to_string),
        limit,
        used_by_limit,
    };
    let reply = resolve(cli)?.deps(
        &opts,
        &options_for(cli, "deps", &args(package, limit, used_by_limit)),
    )?;
    Ok(render::list(cli, reply, render::deps_line, ""))
}

/// The view ID arguments of a question; the API and the MCP tool build the same ones.
pub fn args(
    package: Option<&str>,
    limit: usize,
    used_by_limit: usize,
) -> Vec<(&'static str, String)> {
    vec![
        ("package", package.unwrap_or("").to_string()),
        ("limit", limit.to_string()),
        ("used_by_limit", used_by_limit.to_string()),
    ]
}
