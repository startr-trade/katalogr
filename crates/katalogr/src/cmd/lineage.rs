//! `katalogr lineage <TEXT> [--depth N] [--limit N] [--per-group N] [--expand]` (FEAT-101).

use katalogr_dist::query::{
    DEFAULT_LINEAGE_DEPTH, DEFAULT_LINEAGE_LIMIT, DEFAULT_LINEAGE_PER_GROUP, FOLD_FROM,
    LineageOptions,
};
use katalogr_dist::{DistError, Options};

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

/// The CLI's defaults; the API and MCP share them.
pub const DEFAULT_DEPTH: u32 = DEFAULT_LINEAGE_DEPTH;
pub const DEFAULT_LIMIT: usize = DEFAULT_LINEAGE_LIMIT;
pub const DEFAULT_PER_GROUP: usize = DEFAULT_LINEAGE_PER_GROUP;

/// The facade options for these values. The facade fills the code kinds and, under `expand`,
/// removes folding and sets `per_group` to `limit`.
pub fn options(depth: u32, limit: usize, per_group: usize) -> LineageOptions {
    LineageOptions {
        depth,
        limit,
        per_group,
        code_kinds: Default::default(),
        fold_from: Some(FOLD_FROM),
    }
}

/// The view ID arguments of a question; the MCP tool and the API build the same ones.
pub fn args(
    text: &str,
    depth: u32,
    limit: usize,
    per_group: usize,
    expand: bool,
) -> Vec<(&'static str, String)> {
    vec![
        ("text", text.to_string()),
        ("depth", depth.to_string()),
        ("limit", limit.to_string()),
        ("per-group", per_group.to_string()),
        ("expand", expand.to_string()),
    ]
}

pub fn run(
    cli: &Cli,
    text: &str,
    depth: u32,
    limit: usize,
    per_group: usize,
    expand: bool,
) -> Result<ExitCode, DistError> {
    let opts = Options {
        expand,
        ..options_for(cli, "lineage", &args(text, depth, limit, per_group, expand))
    };
    let reply = resolve(cli)?.lineage(text, options(depth, limit, per_group), &opts)?;
    Ok(render::lineage(cli, reply))
}
