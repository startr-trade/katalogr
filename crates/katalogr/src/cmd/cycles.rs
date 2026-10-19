//! `katalogr cycles [--level L] [--include-inferred] [--members N] [--limit N]`.

use katalogr_dist::DistError;
use katalogr_dist::analytics::CycleOptions;

use crate::cli::{Cli, LevelArg};
use crate::cmd::{graph_options, options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(
    cli: &Cli,
    level: LevelArg,
    include_inferred: bool,
    members: usize,
    limit: usize,
) -> Result<ExitCode, DistError> {
    let k = resolve(cli)?;
    let settings = k.insight_settings();
    let opts = CycleOptions {
        graph: graph_options(level, include_inferred, &settings),
        members,
        limit,
    };
    let reply = k.cycles(&opts, &options(cli))?;
    // Any cycle fails the command, so CI can gate on it.
    Ok(render::insight(cli, reply, "cycles", &render::CYCLES))
}
