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
    no_reports: bool,
) -> Result<ExitCode, DistError> {
    let (files, hist) = scope(since, range, history);
    let k = resolve(cli)?;
    let r = k.index(files, full, hist.as_ref(), &options(cli))?;
    let code = render(cli, &r, &format!("queued {} event(s)", r.queued));
    // Reports that merges asked for are built once the index has drained. Their outcome never
    // changes the exit status of `index`; it goes to standard error.
    if !no_reports && r.pending == 0 && !r.stale {
        match k.diff_requested(&options(cli)) {
            Ok(o) => {
                if o.built > 0 {
                    eprintln!("katalogr: built {} requested report(s)", o.built);
                }
                for w in &o.warnings {
                    eprintln!("katalogr: warning: {w}");
                }
            }
            Err(e) => eprintln!("katalogr: warning: requested reports: {e}"),
        }
    }
    Ok(code)
}
