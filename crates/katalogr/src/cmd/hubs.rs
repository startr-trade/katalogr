//! `katalogr hubs [--level L] [--min-degree D] [--limit N] [--strict]`.

use katalogr_dist::DistError;
use katalogr_dist::analytics::HubOptions;

use crate::cli::{Cli, LevelArg};
use crate::cmd::{graph_options, options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(
    cli: &Cli,
    level: LevelArg,
    min_degree: Option<f64>,
    limit: usize,
    strict: bool,
) -> Result<ExitCode, DistError> {
    let k = resolve(cli)?;
    let settings = k.insight_settings();
    let opts = HubOptions {
        graph: graph_options(level, !strict, &settings),
        min_degree: min_degree.unwrap_or(settings.hub_min_degree),
        limit,
    };
    let reply = k.hubs(&opts, &options(cli))?;
    Ok(render::insight(cli, reply, "hubs", &render::HUBS))
}
