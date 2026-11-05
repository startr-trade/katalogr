//! `katalogr why <file:line>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, location: &str) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.why(
        location,
        &options_for(cli, "why", &[("location", location.to_string())]),
    )?;
    Ok(render::why(cli, reply))
}
