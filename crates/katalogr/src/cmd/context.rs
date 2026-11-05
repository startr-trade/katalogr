//! `katalogr context list|show|add|prune`.

use katalogr_dist::DistError;
use katalogr_dist::contexts::ContextInfo;
use serde_json::json;

use crate::cli::{Cli, ContextAction};
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;

fn to_json(i: &ContextInfo) -> serde_json::Value {
    json!({
        "id": i.id,
        "kind": i.kind,
        "repo": i.repo,
        "commit": i.commit,
        "seed_commit": i.seed_commit,
        "bytes": i.bytes,
        "last_used": i.last_used,
        "current": i.current,
    })
}

fn line(i: &ContextInfo) -> String {
    let mut s = format!("{}{} {}", if i.current { "* " } else { "  " }, i.id, i.kind);
    if let Some(r) = &i.repo {
        s.push_str(&format!(" {r}"));
    }
    if let Some(c) = &i.commit {
        s.push_str(&format!(" @{}", &c[..12.min(c.len())]));
    }
    s.push_str(&format!(" {} bytes", i.bytes));
    s
}

pub fn run(cli: &Cli, action: &ContextAction) -> Result<ExitCode, DistError> {
    let k = resolve(cli)?;
    match action {
        ContextAction::List => {
            let all = k.context_list()?;
            if cli.json {
                let v: Vec<_> = all.iter().map(to_json).collect();
                println!("{}", json!({ "schema": 1, "result": v }));
            } else {
                for i in &all {
                    println!("{}", line(i));
                }
            }
            Ok(ExitCode::Ok)
        }
        ContextAction::Show { id } => {
            let all = k.context_list()?;
            let Some(i) = all.iter().find(|i| &i.id == id) else {
                return Err(DistError::Usage(format!(
                    "unknown context {id}: see `katalogr context list`"
                )));
            };
            if cli.json {
                println!("{}", json!({ "schema": 1, "result": to_json(i) }));
            } else {
                println!("{}", line(i));
            }
            Ok(ExitCode::Ok)
        }
        ContextAction::Add { commit, repo } => {
            let repo = match repo {
                Some(r) => r.clone(),
                None => k.current_repo()?.to_string(),
            };
            let i = k.context_add_commit(&repo, commit, &options(cli))?;
            if cli.json {
                println!("{}", json!({ "schema": 1, "result": to_json(&i) }));
            } else {
                println!("{}", line(&i));
            }
            Ok(ExitCode::Ok)
        }
        ContextAction::Prune { keep } => {
            let keep = keep.unwrap_or_else(|| k.contexts_max());
            let removed = k.context_prune(keep)?;
            if cli.json {
                let ids: Vec<_> = removed.iter().map(|i| i.to_string()).collect();
                println!("{}", json!({ "schema": 1, "result": { "removed": ids } }));
            } else {
                for i in &removed {
                    println!("removed {i}");
                }
            }
            Ok(ExitCode::Ok)
        }
    }
}
