//! `katalogr claude sync [--show]`.

use katalogr_dist::DistError;

use crate::cli::{ClaudeAction, Cli};
use crate::cmd::resolve;
use crate::exit::ExitCode;

pub fn run(cli: &Cli, action: &ClaudeAction) -> Result<ExitCode, DistError> {
    match action {
        ClaudeAction::Sync { show } => {
            let k = resolve(cli)?;
            let (outcome, path) = katalogr_dist::claude_sync(k.repo_root()?.as_std_path(), *show)?;
            if *show {
                println!("{}", katalogr_dist::claude_block());
            } else {
                println!("{} {}", outcome.as_str(), path.display());
            }
            Ok(ExitCode::Ok)
        }
    }
}
