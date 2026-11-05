//! `katalogr why <file:line>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, location: &str, no_use: bool) -> Result<ExitCode, DistError> {
    let limit = (!no_use).then_some(katalogr_dist::query::DEFAULT_USE_LIMIT);
    let reply = resolve(cli)?.why(location, limit, &options(cli))?;
    Ok(render::why(cli, reply))
}
