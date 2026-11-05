//! `katalogr context list|show|add|prune`.

use katalogr_dist::DistError;
use katalogr_dist::contexts::ContextInfo;
use serde_json::json;

use crate::cli::{Cli, ContextAction};
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;
use katalogr_dist::analytics::context_diff::{ContextConflicts, ContextDiff};

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
        ContextAction::Diff { a, b, limit } => {
            let reply = k.context_diff(a, b, *limit, &options(cli))?;
            Ok(render::plain(
                cli,
                reply,
                "context-diff",
                diff_lines,
                |_| false,
            ))
        }
        ContextAction::Conflicts { a, b } => {
            let reply = k.context_conflicts(a, b, &options(cli))?;
            Ok(render::plain(
                cli,
                reply,
                "context-conflicts",
                conflict_lines,
                |c| !c.conflicts.is_empty(),
            ))
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

fn diff_lines(d: &ContextDiff) -> Vec<String> {
    let mut out = Vec::new();
    for l in &d.links_added {
        out.push(format!("+ {} {} {}", l.from, l.kind, l.to));
    }
    for l in &d.links_removed {
        out.push(format!("- {} {} {}", l.from, l.kind, l.to));
    }
    for x in &d.dangling_added {
        out.push(format!(
            "+ dangling {} {} wants {}",
            x.from.name,
            x.kind,
            x.wanted.join(", ")
        ));
    }
    for x in &d.dangling_removed {
        out.push(format!(
            "- dangling {} {} wants {}",
            x.from.name,
            x.kind,
            x.wanted.join(", ")
        ));
    }
    for m in &d.metrics {
        out.push(format!("{} {} -> {} ({:+})", m.name, m.a, m.b, m.delta));
    }
    out
}

fn conflict_lines(c: &ContextConflicts) -> Vec<String> {
    c.conflicts
        .iter()
        .map(|n| format!("conflict {n}"))
        .collect()
}
