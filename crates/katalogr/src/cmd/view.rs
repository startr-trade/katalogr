//! `katalogr view <op> [args] [--at P] [--fresh] [--json]` (FEAT-088): a generated view,
//! answered from the store by its view ID when it is there. One facade call; the bytes it
//! returns are what `GET /api/v1/views/<op>` sends.

use katalogr_dist::render::ViewSpec;
use katalogr_dist::{DistError, FreshCheck};

use crate::cli::{Cli, ViewAction};
use crate::cmd::render::line_of;
use crate::cmd::view_doc;
use crate::cmd::{options, resolve, resolve_with_snapshots};
use crate::exit::ExitCode;
use crate::render;

pub fn run(
    cli: &Cli,
    action: Option<&ViewAction>,
    fresh: bool,
    words: &[String],
) -> Result<ExitCode, DistError> {
    match action {
        Some(ViewAction::Check { path }) => return view_doc::check(cli, path),
        Some(ViewAction::Snapshot { docs, out }) => return view_doc::snapshot(cli, docs, out),
        Some(ViewAction::Timeline { snapshots, limit }) => {
            let k = resolve_with_snapshots(cli, snapshots)?;
            return Ok(render::timeline(cli, k.timeline(*limit)?));
        }
        Some(ViewAction::Stored { id, snapshots }) => {
            let k = resolve_with_snapshots(cli, snapshots)?;
            return Ok(render::view_bytes(cli, k.stored_view(id)?));
        }
        None => {}
    }
    if words.is_empty() {
        return Err(DistError::Usage(
            "view wants an op (structure, contracts, callers, impact, path, cycles), `check` or `snapshot`"
                .to_string(),
        ));
    }
    // `--fresh` may come after the op, with the view's own flags.
    let fresh = fresh || words.iter().any(|w| w == "--fresh");
    let words: Vec<String> = words.iter().filter(|w| *w != "--fresh").cloned().collect();
    let spec = ViewSpec::parse(&line_of(&words)).map_err(|e| DistError::Usage(e.to_string()))?;
    let k = resolve(cli)?;
    let opts = options(cli);
    if fresh {
        return Ok(match k.view_fresh(&spec, &opts)? {
            FreshCheck::Same => state(cli, "same", None),
            FreshCheck::NotStored => state(cli, "not-stored", None),
            FreshCheck::Different { view_id } => {
                eprintln!("katalogr: stored view {view_id} differs from a fresh computation");
                state(cli, "different", Some(&view_id));
                ExitCode::Error
            }
        });
    }
    Ok(render::view_bytes(cli, k.view(&spec, &opts)?))
}

/// What `--fresh` found: a JSON line with `--json`, else a word.
fn state(cli: &Cli, found: &str, id: Option<&str>) -> ExitCode {
    if cli.json {
        println!(
            "{}",
            serde_json::json!({"schema": 1, "fresh": found, "view_id": id})
        );
    } else {
        println!("fresh: {found}");
    }
    ExitCode::Ok
}
