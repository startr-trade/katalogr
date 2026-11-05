//! `katalogr status`: the index watermark, pending changes, staleness and the context read.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render::{emit, json};

pub fn run(cli: &Cli) -> Result<ExitCode, DistError> {
    let k = resolve(cli)?;
    let r = k.status(&options(cli))?;
    let context = k.context_name();
    if cli.json {
        return Ok(emit(&json::status(&r, context)));
    }
    for w in &r.warnings {
        eprintln!("katalogr: warning: {w}");
    }
    println!(
        "index status; watermark {}, {} pending{}; context {context}",
        r.watermark,
        r.pending,
        if r.stale { " (stale)" } else { "" }
    );
    Ok(if r.timed_out {
        ExitCode::Stale
    } else {
        ExitCode::Ok
    })
}
