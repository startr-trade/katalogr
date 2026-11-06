//! `katalogr answers check [--limit N] [--repair]`, `answers stats` and `answers prune
//! [--max-mib N]` (FEAT-096 R-10, R-12).

use katalogr_dist::DistError;

use crate::cli::{AnswersAction, Cli};
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;

pub fn run(cli: &Cli, action: &AnswersAction) -> Result<ExitCode, DistError> {
    let k = resolve(cli)?;
    match action {
        AnswersAction::Check { limit, repair } => {
            let r = k.answers_check(*limit, *repair, &options(cli))?;
            if cli.json {
                println!("{}", serde_json::to_string(&r).expect("serialise report"));
            } else if !r.exact {
                println!("not exact: the checkout's position could not be held; nothing checked");
            } else {
                println!(
                    "checked {}: {} match, {} differ",
                    r.checked,
                    r.matched,
                    r.mismatched.len()
                );
                for m in &r.mismatched {
                    println!(
                        "  {} ({}): first difference at byte {}",
                        m.view_id, m.op, m.first_difference
                    );
                }
                println!(
                    "skipped: {} at another position, {} under another build or index, {} unknown",
                    r.skipped_other_position, r.skipped_other_validity, r.skipped_unknown_op
                );
                if r.removed > 0 {
                    println!("removed {}", r.removed);
                }
            }
            Ok(if !r.exact {
                ExitCode::Stale
            } else if r.mismatched.is_empty() {
                ExitCode::Ok
            } else {
                ExitCode::Error
            })
        }
        AnswersAction::Stats => {
            let s = k.answers_stats()?;
            if cli.json {
                println!("{}", serde_json::to_string(&s).expect("serialise stats"));
            } else {
                println!(
                    "{} stored answers, {} bytes (limit {} MiB)",
                    s.entries, s.bytes, s.max_mib
                );
                for (op, n) in &s.by_op {
                    println!("  {op}: {n}");
                }
            }
            Ok(ExitCode::Ok)
        }
        AnswersAction::Prune { max_mib } => {
            let r = k.answers_prune(*max_mib)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({"removed": r.removed, "bytes_left": r.bytes_left})
                );
            } else {
                println!("removed {}, {} bytes left", r.removed, r.bytes_left);
            }
            Ok(ExitCode::Ok)
        }
    }
}
