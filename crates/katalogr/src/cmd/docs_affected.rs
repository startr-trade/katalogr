//! `katalogr docs-affected [--since <rev>] [--at <position>] [--kind K]... [--limit N]`.

use katalogr_dist::{DistError, DocsAffectedParams};

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(
    cli: &Cli,
    since: Option<&str>,
    kinds: &[String],
    limit: usize,
) -> Result<ExitCode, DistError> {
    let params = DocsAffectedParams {
        kinds: kinds.to_vec(),
        limit,
    };
    let reply = resolve(cli)?.docs_affected(
        since,
        &params,
        &options_for(cli, "docs-affected", &args(since, kinds, limit)),
    )?;
    Ok(render::docs_affected(cli, reply))
}

/// The view ID arguments of a question; the MCP tool builds the same ones.
pub fn args(since: Option<&str>, kinds: &[String], limit: usize) -> Vec<(&'static str, String)> {
    vec![
        ("since", since.unwrap_or("").to_string()),
        ("kinds", kinds.join(",")),
        ("limit", limit.to_string()),
    ]
}
