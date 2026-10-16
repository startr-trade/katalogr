//! `katalogr project init|add-root`.

use katalogr_dist::DistError;

use crate::cli::{Cli, ProjectAction};
use crate::cmd::{path, resolve};
use crate::exit::ExitCode;

pub fn run(cli: &Cli, action: &ProjectAction) -> Result<ExitCode, DistError> {
    match action {
        ProjectAction::Init { name, repos, mark } => {
            let kindex = cli
                .kindex
                .clone()
                .or_else(|| {
                    std::env::var("KATALOGR_KINDEX")
                        .ok()
                        .filter(|s| !s.is_empty())
                })
                .ok_or_else(|| DistError::Usage("project init needs --kindex <dir>".into()))?;
            let repos = repos
                .iter()
                .map(|(n, p)| (n.clone(), path(p).into()))
                .collect();
            let c = katalogr_dist::project_init(path(&kindex), name, repos, *mark)?;
            println!("project {name} created at {}", c.project_dir.display());
            for m in c.markers {
                println!("marker written: {}", m.display());
            }
            Ok(ExitCode::Ok)
        }
        ProjectAction::AddRoot { name, path: p } => {
            resolve(cli)?.add_root(name, path(p))?;
            println!("root of {name} recorded");
            Ok(ExitCode::Ok)
        }
    }
}
