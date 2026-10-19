//! `katalogr skill install [--scope user|repo]`.

use katalogr_dist::DistError;

use crate::cli::{Cli, SkillAction, SkillScope};
use crate::cmd::resolve;
use crate::exit::ExitCode;

pub fn run(cli: &Cli, action: &SkillAction) -> Result<ExitCode, DistError> {
    match action {
        SkillAction::Install { scope } => {
            let target = if *scope == SkillScope::Repo {
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
