//! `katalogr mirrors [--kind <contract kind>]...`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, kinds: &[String]) -> Result<ExitCode, DistError> {
    let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
    let reply = resolve(cli)?.mirrors(
        &kinds,
        &options_for(cli, "mirrors", &[("kinds", kinds.join(","))]),
    )?;
    Ok(render::mirrors(cli, reply))
}
