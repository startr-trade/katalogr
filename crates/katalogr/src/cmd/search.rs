//! `katalogr search <words...>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options_with, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(
    cli: &Cli,
    terms: &[String],
    limit: usize,
    diagnostic: bool,
) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.search(&terms.join(" "), limit, &options_with(cli, diagnostic))?;
    Ok(render::list(cli, reply, render::search_line, "search"))
}
