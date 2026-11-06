//! `katalogr deps [--package NAME] [--limit N] [--used-by-limit N]`.

use katalogr_dist::DistError;
use katalogr_dist::query::DepsOptions;

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
        &options_for(
            cli,
            "deps",
            &[
                ("package", package.unwrap_or("").to_string()),
                ("limit", limit.to_string()),
                ("used_by_limit", used_by_limit.to_string()),
            ],
        ),
    )?;
    Ok(render::list(cli, reply, render::deps_line, ""))
}
