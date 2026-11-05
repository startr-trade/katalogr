//! `katalogr map`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options_for_diag, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, diagnostic: bool) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.map(&options_for_diag(cli, "map", &[], diagnostic))?;
    Ok(render::list(cli, reply, render::map_line, "map"))
}
