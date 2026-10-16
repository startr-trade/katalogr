//! `katalogr kindex init <dir>`.

use katalogr_dist::DistError;

use crate::cli::KindexAction;
use crate::cmd::path;
use crate::exit::ExitCode;

pub fn run(action: &KindexAction) -> Result<ExitCode, DistError> {
    match action {
        KindexAction::Init { dir } => {
            let d = katalogr_dist::kindex_init(path(dir))?;
            println!("kindex ready at {}", d.display());
            Ok(ExitCode::Ok)
        }
    }
}
