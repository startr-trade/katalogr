//! `katalogr callers <text> [--depth N]`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, text: &str, depth: u32) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.callers(text, depth, &options(cli))?;
    Ok(render::list(cli, reply, render::caller_line, "callers"))
}
