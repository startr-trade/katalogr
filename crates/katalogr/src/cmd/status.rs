//! `katalogr status`: the index watermark, pending changes and staleness.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, render, resolve};
use crate::exit::ExitCode;

pub fn run(cli: &Cli) -> Result<ExitCode, DistError> {
    let r = resolve(cli)?.status(&options(cli))?;
    Ok(render(cli, &r, "index status"))
}
