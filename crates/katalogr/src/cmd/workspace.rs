//! `katalogr workspace status|map|add|locate` (FEAT-020).

use katalogr_dist::DistError;

use crate::cli::{Cli, WorkspaceAction};
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, action: &WorkspaceAction) -> Result<ExitCode, DistError> {
    let k = resolve(cli)?;
    match action {
        WorkspaceAction::Status => {
            let peers = k.workspace_status(&options(cli))?;
            Ok(render::workspace_status(cli, peers))
        }
        WorkspaceAction::Map => {
            let reply = k.workspace_map(&options(cli))?;
            Ok(render::workspace_map(cli, reply))
        }
        WorkspaceAction::Add { peer, alias } => {
            k.workspace_add(peer, alias.as_deref())?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "schema": 1,
                        "added": { "project": peer, "kindex": alias },
                    })
                );
            } else {
                match alias {
                    Some(a) => println!("peer {a}/{peer} added"),
                    None => println!("peer {peer} added"),
                }
            }
            Ok(ExitCode::Ok)
        }
        WorkspaceAction::Locate { alias, dir } => {
            k.workspace_locate(alias, std::path::Path::new(dir))?;
            if cli.json {
                println!("{}", serde_json::json!({ "schema": 1, "located": alias }));
            } else {
                println!("kindex {alias} located on this machine");
            }
            Ok(ExitCode::Ok)
        }
    }
}
