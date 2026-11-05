//! `katalogr map`.

use katalogr_dist::{DistError, Options};

use crate::cli::Cli;
use crate::cmd::{options_for_diag, ranking, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, diagnostic: bool, neutral: bool) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.map(&Options {
        ranking: ranking(neutral),
        ..options_for_diag(cli, "map", &[], diagnostic)
    })?;
    Ok(render::list(cli, reply, render::map_line, "map"))
}
