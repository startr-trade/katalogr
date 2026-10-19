//! `katalogr refs <text>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options_with, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, text: &str, diagnostic: bool) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.refs(text, &options_with(cli, diagnostic))?;
    Ok(render::list(cli, reply, render::ref_line, "refs"))
}
