//! `katalogr audit record` and `katalogr audit verify [<record>]` (FEAT-094 R-14).

use katalogr_dist::{AuditOutcome, DistError};

use crate::cli::{AuditAction, Cli};
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;

pub fn run(cli: &Cli, action: &AuditAction) -> Result<ExitCode, DistError> {
    let k = resolve(cli)?;
    match action {
        AuditAction::Record => {
            let record = k.audit_record(&options(cli))?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string(&record).expect("serialise record")
                );
            } else {
                println!(
                    "recorded audit/{}: facts {}, graph {}, {} repos",
                    record.file_name(),
                    short(&record.facts_version),
                    short(&record.graph_digest),
                    record.positions.len()
                );
            }
            Ok(ExitCode::Ok)
        }
        AuditAction::Verify { record } => {
            let outcome = k.audit_verify(record.as_deref().map(std::path::Path::new), &options(cli))?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string(&outcome).expect("serialise outcome")
                );
            } else {
                print_outcome(&outcome);
            }
            Ok(match outcome {
                AuditOutcome::Match => ExitCode::Ok,
                AuditOutcome::Mismatch { .. } => ExitCode::Error,
                AuditOutcome::NotComparable { .. } | AuditOutcome::NotAtPosition { .. } => {
                    ExitCode::Stale
                }
            })
        }
    }
}

fn short(hex: &str) -> &str {
    &hex[..hex.len().min(12)]
}

fn print_outcome(o: &AuditOutcome) {
    match o {
        AuditOutcome::Match => println!("match"),
        AuditOutcome::Mismatch { expected, found } => {
            println!("mismatch: the record has {expected}, the index has {found}");
        }
        AuditOutcome::NotComparable {
            record_facts,
            current_facts,
        } => println!(
            "not comparable: the record was built under facts {}, this index under {}",
            short(record_facts),
            short(current_facts)
        ),
        AuditOutcome::NotAtPosition {
            want,
            have,
            command,
        } => {
            println!("not at the recorded position; to index it, run: {command}");
            for p in want {
                println!("  want {}: {}", p.repo, text(p));
            }
            for p in have {
                println!("  have {}: {}", p.repo, text(p));
            }
        }
    }
}

fn text(p: &katalogr_dist::PositionText) -> String {
    match (&p.commit, &p.tree) {
        (Some(c), None) => c.clone(),
        (Some(c), Some(t)) => format!("{c}+{t}"),
        (None, Some(t)) => format!("none+{t}"),
        (None, None) => "none".to_string(),
    }
}
