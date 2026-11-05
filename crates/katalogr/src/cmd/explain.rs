//! `katalogr explain <symbol>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, symbol: &str, per_side: usize) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.explain(
        symbol,
        per_side,
        &options_for(
            cli,
            "explain",
            &[
                ("symbol", symbol.to_string()),
                ("per_side", per_side.to_string()),
            ],
        ),
    )?;
    Ok(render::explain(cli, reply))
}
