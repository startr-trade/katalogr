//! `katalogr view check <path>` and `katalogr view snapshot --docs <dir> --out <dir>`
//! (FEAT-035): thin calls to `katalogr_dist::doc_views`.

use std::path::Path;

use katalogr_dist::DistError;
use katalogr_dist::doc_views::{SnapshotRequest, check_blocks};

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;

/// Report each query block under `path` that does not parse. Needs no project.
pub fn check(cli: &Cli, path: &Path) -> Result<ExitCode, DistError> {
    let problems = check_blocks(path)?;
    if cli.json {
        let list: Vec<serde_json::Value> = problems
            .iter()
            .map(|p| {
                serde_json::json!({
                    "file": p.file, "line": p.line, "source": p.source, "error": p.error,
                })
            })
            .collect();
        println!("{}", serde_json::json!({"schema": 1, "problems": list}));
    } else {
        for p in &problems {
            println!("{}:{}: {}", p.file, p.line, p.error);
        }
        if problems.is_empty() {
            println!("all query blocks parse");
        }
    }
    Ok(if problems.is_empty() {
        ExitCode::Ok
    } else {
        ExitCode::Error
    })
}

/// Write the snapshot, then print its summary or, with `--json`, its index.
pub fn snapshot(cli: &Cli, docs: &Path, out: &Path) -> Result<ExitCode, DistError> {
    let k = resolve(cli)?;
    let index = k.snapshot(&SnapshotRequest {
        docs: docs.to_path_buf(),
        out: out.to_path_buf(),
        options: options(cli),
    })?;
    if cli.json {
        println!(
            "{}",
            serde_json::to_string(&index).expect("serialise the index")
        );
    } else {
        let views: std::collections::BTreeSet<&str> =
            index.blocks.iter().map(|b| b.view_id.as_str()).collect();
        println!(
            "snapshot: {} block(s), {} view(s), {} repo(s)",
            index.blocks.len(),
            views.len(),
            index.repos.len()
        );
    }
    Ok(ExitCode::Ok)
}
