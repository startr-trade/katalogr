//! `katalogr skill install [--project]`.

use katalogr_dist::DistError;

use crate::cli::{Cli, SkillAction};
use crate::cmd::resolve;
use crate::exit::ExitCode;

pub fn run(cli: &Cli, action: &SkillAction) -> Result<ExitCode, DistError> {
    match action {
        SkillAction::Install { in_repo } => {
            let target = if *in_repo {
                katalogr_dist::skill_path(resolve(cli)?.repo_root()?.as_std_path())
            } else {
                katalogr_dist::user_skill_path()?
            };
            let outcome = katalogr_dist::skill_install(&target)?;
            println!("{} {}", outcome.as_str(), target.display());
            Ok(ExitCode::Ok)
        }
    }
}
