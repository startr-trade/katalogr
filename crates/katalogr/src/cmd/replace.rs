//! `katalogr replace <symbol> --file <path>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

use crate::cmd::path;

pub fn run(cli: &Cli, symbol: &str, file: &str) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.replace(symbol, path(file), &options(cli))?;
    Ok(render::replace(cli, reply))
}
