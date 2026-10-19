//! `katalogr why <file:line>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, location: &str) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.why(location, &options(cli))?;
    Ok(render::why(cli, reply))
}
