//! `katalogr index [--since <rev> | --range a..b] [--full]`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, render, resolve};
use crate::exit::ExitCode;

pub fn run(
    cli: &Cli,
    since: Option<&String>,
    range: Option<&String>,
    full: bool,
) -> Result<ExitCode, DistError> {
    let r = resolve(cli)?.index(range.or(since).cloned(), full, &options(cli))?;
    Ok(render(cli, &r, &format!("queued {} event(s)", r.queued)))
}
