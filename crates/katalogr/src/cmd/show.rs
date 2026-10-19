//! `katalogr show <symbol> [--signature]`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, symbol: &str, signature: bool) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.show(symbol, signature, &options(cli))?;
    Ok(render::show(cli, reply))
}
