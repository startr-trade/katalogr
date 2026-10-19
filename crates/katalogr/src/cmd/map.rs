//! `katalogr map`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.map(&options(cli))?;
    Ok(render::list(cli, reply, render::map_line, "map"))
}
