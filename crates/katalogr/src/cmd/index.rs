//! `katalogr index [--since <rev> | --range a..b] [--full] [--history]`.

use katalogr_dist::{DistError, HistoryScope};

use crate::cli::Cli;
use crate::cmd::{options, render, resolve};
use crate::exit::ExitCode;

/// Map `index` arguments to the dist call's (range, history) exactly as the CLI does: with
/// `history`, `since` bounds the commit walk; otherwise `range.or(since)` is the file range.
pub fn scope(
    since: Option<&String>,
    range: Option<&String>,
    history: bool,
) -> (Option<String>, Option<HistoryScope>) {
    if history {
        (
            None,
            Some(HistoryScope {
                since: since.cloned(),
            }),
        )
    } else {
        (range.or(since).cloned(), None)
    }
}

pub fn run(
    cli: &Cli,
    since: Option<&String>,
    range: Option<&String>,
    full: bool,
    history: bool,
) -> Result<ExitCode, DistError> {
    let (files, hist) = scope(since, range, history);
    let r = resolve(cli)?.index(files, full, hist.as_ref(), &options(cli))?;
    Ok(render(cli, &r, &format!("queued {} event(s)", r.queued)))
}
