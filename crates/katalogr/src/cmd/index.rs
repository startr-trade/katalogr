//! `katalogr index [--since <rev> | --range a..b] [--full] [--history]`.

use katalogr_dist::{DistError, HistoryScope};

use crate::cli::Cli;
use crate::cmd::{options, render, resolve};
use crate::exit::ExitCode;

pub fn run(
    cli: &Cli,
    since: Option<&String>,
    range: Option<&String>,
    full: bool,
    history: bool,
) -> Result<ExitCode, DistError> {
    // With `--history`, `--since` bounds the commit walk instead of the file changes.
    let (files, scope) = if history {
        (
            None,
            Some(HistoryScope {
                since: since.cloned(),
            }),
        )
    } else {
        (range.or(since).cloned(), None)
    };
    let r = resolve(cli)?.index(files, full, scope.as_ref(), &options(cli))?;
    Ok(render(cli, &r, &format!("queued {} event(s)", r.queued)))
}
