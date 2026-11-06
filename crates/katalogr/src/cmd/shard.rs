//! `katalogr shard show <repo>:<path> [--commit <rev>]` (FEAT-094 R-13).

use katalogr_dist::DistError;

use crate::cli::{Cli, ShardAction};
use crate::cmd::resolve;
use crate::exit::ExitCode;

pub fn run(cli: &Cli, action: &ShardAction) -> Result<ExitCode, DistError> {
    match action {
        ShardAction::Show { target, commit } => {
            let Some((repo, path)) = target.split_once(':') else {
                return Err(DistError::Usage(format!(
                    "{target:?}: expected <repo>:<path>"
                )));
            };
            let text = resolve(cli)?.shard_show(repo, path, commit.as_deref())?;
            print!("{text}");
            Ok(ExitCode::Ok)
        }
    }
}
