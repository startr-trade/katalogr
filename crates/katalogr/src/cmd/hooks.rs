//! `katalogr hooks install|uninstall|status [--global]`.

use katalogr_dist::DistError;
use katalogr_dist::hooks::{install, status, uninstall};

use crate::cli::{Cli, HooksAction};
use crate::cmd::overrides;
use crate::exit::ExitCode;

pub fn run(cli: &Cli, action: &HooksAction) -> Result<ExitCode, DistError> {
    let cwd =
        std::env::current_dir().map_err(|e| DistError::Usage(format!("current directory: {e}")))?;
    let ov = overrides(cli);
    let lines = match action {
        HooksAction::Install { global } => install(&cwd, &ov, *global)?,
        HooksAction::Uninstall { global } => uninstall(&cwd, &ov, *global)?,
        HooksAction::Status => status(&cwd, &ov)?,
    };
    if cli.json {
        println!("{}", serde_json::json!({ "lines": lines }));
    } else {
        for l in lines {
            println!("{l}");
        }
    }
    Ok(ExitCode::Ok)
}
