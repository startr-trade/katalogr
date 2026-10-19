//! `katalogr communities [--level L] [--seed S] [--resolution G] [--members N] [--limit N] [--of NAME] [--strict]`.

use katalogr_dist::DistError;
use katalogr_dist::analytics::CommunityOptions;

use crate::cli::{Cli, LevelArg};
use crate::cmd::{graph_options, options, resolve};
use crate::exit::ExitCode;
use crate::render;

#[allow(clippy::too_many_arguments)]
pub fn run(
    cli: &Cli,
    level: LevelArg,
    seed: Option<u64>,
    resolution: Option<f64>,
    members: usize,
    limit: usize,
    of: Option<&str>,
    strict: bool,
) -> Result<ExitCode, DistError> {
    let k = resolve(cli)?;
    let settings = k.insight_settings();
    let opts = CommunityOptions {
        graph: graph_options(level, !strict, &settings),
        seed: seed.unwrap_or(settings.seed),
        resolution: resolution.unwrap_or(settings.resolution),
        min_size: settings.community_min_size,
        members,
        limit,
        of: of.map(str::to_string),
    };
    let reply = k.communities(&opts, &options(cli))?;
    Ok(render::insight(
        cli,
        reply,
        "communities",
        &render::COMMUNITIES,
    ))
}
