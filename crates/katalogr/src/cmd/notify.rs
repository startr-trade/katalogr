//! `katalogr notify [--deleted] <paths…>` and `katalogr notify --from-git <hook> [args…]`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, overrides, render, resolve};
use crate::exit::ExitCode;

pub fn run(
    cli: &Cli,
    deleted: bool,
    from_git: Option<&str>,
    paths: &[String],
) -> Result<ExitCode, DistError> {
    if let Some(hook) = from_git {
        // Called by git: never fail and never block it.
        if let Ok(cwd) = std::env::current_dir() {
            katalogr_dist::hooks::notify_from_git(&cwd, &overrides(cli), hook, paths);
        }
        return Ok(ExitCode::Ok);
    }
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
