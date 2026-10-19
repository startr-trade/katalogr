//! `katalogr explain <symbol>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, symbol: &str, per_side: usize) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.explain(symbol, per_side, &options(cli))?;
    Ok(render::explain(cli, reply))
}
