//! `katalogr refs <text>`.

use katalogr_dist::{DistError, Options};

use crate::cli::Cli;
use crate::cmd::{options_with, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, text: &str, diagnostic: bool, expand: bool) -> Result<ExitCode, DistError> {
    let opts = Options {
        expand,
        ..options_with(cli, diagnostic)
    };
    let reply = resolve(cli)?.refs(text, &opts)?;
    Ok(render::list_folded(cli, reply, render::ref_line, "refs"))
}
