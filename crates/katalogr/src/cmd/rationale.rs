//! `katalogr rationale [SYMBOL] [--path P] [--hubs N] [--limit N]`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(
    cli: &Cli,
    symbol: Option<&str>,
    path: Option<&str>,
    hubs: Option<usize>,
    limit: usize,
) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.rationale(
        symbol,
        path,
        hubs,
        limit,
        &options_for(
            cli,
            "rationale",
            &[
                ("symbol", format!("{symbol:?}")),
                ("path", format!("{path:?}")),
                ("hubs", format!("{hubs:?}")),
                ("limit", limit.to_string()),
            ],
        ),
    )?;
    Ok(render::insight(cli, reply, "rationale", &render::RATIONALE))
}
