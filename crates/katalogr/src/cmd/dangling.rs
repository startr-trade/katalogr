//! `katalogr dangling [--kind <link kind>]...`; `contract` selects one-sided contracts.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, kinds: &[String]) -> Result<ExitCode, DistError> {
    let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
    let reply = resolve(cli)?.dangling(&kinds, &options(cli))?;
    Ok(render::dangling(cli, reply))
}
