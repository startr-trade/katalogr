//! `katalogr hook pre-read|post-search`: Claude Code hook entry points. They read the hook JSON
//! on stdin, print at most one JSON object, and always exit 0 (fail open).

use std::io::Read;

use katalogr_dist::DistError;
use katalogr_dist::hooks::{HookKind, run_hook};

use crate::cli::{Cli, HookAction};
use crate::cmd::overrides;
use crate::exit::ExitCode;

pub fn run(cli: &Cli, action: &HookAction) -> Result<ExitCode, DistError> {
    let kind = match action {
        HookAction::PreRead => HookKind::PreRead,
        HookAction::PostSearch => HookKind::PostSearch,
    };
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return Ok(ExitCode::Ok);
    }
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(ExitCode::Ok);
    };
    if let Some(out) = run_hook(kind, &input, &cwd, &overrides(cli)) {
        println!("{out}");
    }
    Ok(ExitCode::Ok)
}
