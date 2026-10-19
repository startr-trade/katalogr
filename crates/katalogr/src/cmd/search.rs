//! `katalogr search <words...>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, terms: &[String], limit: usize) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.search(&terms.join(" "), limit, &options(cli))?;
    Ok(render::list(cli, reply, render::search_line, "search"))
}
