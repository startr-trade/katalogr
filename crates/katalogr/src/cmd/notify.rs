//! `katalogr notify [--deleted] <paths…>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, render, resolve};
use crate::exit::ExitCode;

pub fn run(cli: &Cli, deleted: bool, paths: &[String]) -> Result<ExitCode, DistError> {
    let k = resolve(cli)?;
    let queued = k.notify(paths, deleted)?;
    if !cli.json {
        println!("queued {queued} change(s)");
        return Ok(ExitCode::Ok);
    }
    // Only the JSON form reads the index state; the hook path stays cheap.
    let mut r = k.status(&options(cli))?;
    r.queued = queued;
    render(cli, &r, "");
    Ok(ExitCode::Ok)
}
