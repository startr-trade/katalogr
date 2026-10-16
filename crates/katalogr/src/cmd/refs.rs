//! `katalogr refs <text>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, text: &str) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.refs(text, &options(cli))?;
    Ok(render::list(cli, reply, render::ref_line))
}
