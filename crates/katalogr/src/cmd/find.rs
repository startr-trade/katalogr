//! `katalogr find <text>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, text: &str) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.find(
        text,
        &options_for(cli, "find", &[("text", text.to_string())]),
    )?;
    Ok(render::list(cli, reply, render::symbol_line, "find"))
}
