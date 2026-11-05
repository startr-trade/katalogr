//! The JSON answers of the query commands, before any I/O (ADR-0023, FEAT-067).
//!
//! Each function turns a facade reply into a [`JsonAnswer`]: exactly what the CLI prints with
//! `--json`, the exit code it returns and the lines it writes on stderr. The CLI prints the
//! answer ([`super::emit`]); the MCP server wraps it. Both come from this one renderer, so the
//! two surfaces cannot drift apart.

use katalogr_dist::analytics::Itemized;
use katalogr_dist::eval::{EvalReport, SweepReport};
use katalogr_dist::query::{
    DanglingRef, Envelope, Explain, Fold, HintSource, MirrorDrift, Partial, RESULT_SCHEMA_VERSION,
    ReplaceResult, Serialize, ShowResult, SymbolHit, Trace, Truncation, WhyResult, budget, cap,
    hints,
};
use katalogr_dist::{DistError, Outcome, Reply, Report};

use crate::exit::ExitCode;

/// What one query yields in JSON mode, before any I/O. The CLI prints it; MCP wraps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonAnswer {
    /// The exit code the CLI returns for this answer.
    pub exit: ExitCode,
    /// Exactly the line the CLI prints on stdout with `--json` (compact JSON, no trailing
    /// newline): the envelope; for ambiguity, the candidates' `find` envelope; for
    /// index/status, `Report::to_json()`. `None` when the CLI prints nothing on stdout.
    pub stdout: Option<String>,
    /// The one-line message the CLI prints on stderr after `katalogr: ` (no match, stale
    /// file, ambiguity count, error). `None` when there is none.
    pub message: Option<String>,
    /// Non-fatal notices the CLI prints as `katalogr: warning: …` (skipped repos, stale answer).
    pub warnings: Vec<String>,
    /// `diagnostic:` lines the CLI prints on stderr when an answer has no envelope to carry
    /// the trace (no match, stale file).
    pub diagnostic: Vec<String>,
}

impl JsonAnswer {
    fn new(exit: ExitCode) -> Self {
        JsonAnswer {
            exit,
            stdout: None,
            message: None,
            warnings: Vec::new(),
            diagnostic: Vec::new(),
        }
    }
}

pub(super) fn envelope<T>(
    report: &Report,
    truncated: Option<Truncation>,
    partial: Vec<Partial>,
    result: T,
    next: Vec<String>,
    diagnostic: Option<Trace>,
) -> Envelope<T> {
    Envelope {
        schema: RESULT_SCHEMA_VERSION,
        watermark: report.watermark,
        pending: report.pending,
        stale: report.stale,
        truncated,
        partial,
        result,
        next,
        diagnostic,
        folded: Vec::new(),
    }
}

/// The marker of an answer a token budget cut: `kept` of `kept + omitted` items remain.
pub(super) fn budget_partial(budget: u64, kept: usize, omitted: usize) -> Partial {
    Partial::new(
        cap::BUDGET,
        budget,
        kept as u64,
        Some((kept + omitted) as u64),
    )
}

/// Describe the packing of `strings` into `budget` in `trace`, on the strings given to
/// `fit_budget`, so the trace describes the packing that happened.
pub(super) fn set_packing(trace: &mut Option<Trace>, strings: &[String], budget: u64) {
    if let Some(t) = trace {
        let pack = budget::pack_trace(strings, budget);
        t.add_omitted(
            "pack_entries",
            pack.kept
                .saturating_sub(katalogr_dist::query::DIAG_PACK as u64),
        );
        t.packing = Some(pack);
    }
}

/// The exit code of a reply: ambiguity, no match, a stale file, or a timed-out wait. A timed-out
/// wait is stale whatever the outcome, since a no-match or ambiguous answer may only reflect
/// what was not indexed yet; the outcome stays in the output (FEAT-080 R-5).
pub(super) fn exit_for<T>(reply: &Reply<T>) -> ExitCode {
    match &reply.outcome {
        Outcome::Ambiguous(_) | Outcome::NoMatch(_) if reply.report.timed_out => ExitCode::Stale,
        Outcome::Ambiguous(_) => ExitCode::Ambiguous,
        Outcome::NoMatch(_) => ExitCode::NoMatch,
        Outcome::StaleFile(_) => ExitCode::Stale,
        Outcome::Found(_) if reply.report.timed_out => ExitCode::Stale,
        Outcome::Found(_) => ExitCode::Ok,
    }
}

/// The warnings of a read: the report's, then the stale-answer notice.
pub(super) fn stale_warnings(report: &Report) -> Vec<String> {
    let mut w = report.warnings.clone();
    if report.stale {
        w.push(format!(
            "answer may be stale ({} change(s) pending)",
            report.pending
        ));
    }
    w
}

fn line<T: Serialize>(e: &Envelope<T>) -> String {
    serde_json::to_string(e).expect("serialise envelope")
}

/// A found answer: the envelope on stdout and the read's warnings.
fn found(exit: ExitCode, report: &Report, stdout: String) -> JsonAnswer {
    JsonAnswer {
        stdout: Some(stdout),
        warnings: stale_warnings(report),
        ..JsonAnswer::new(exit)
    }
}

/// The envelope of a list within the budget, with the hints for `command` (none when empty).
fn list_envelope<T: Serialize>(
    report: &Report,
    items: Vec<T>,
    mut partial: Vec<Partial>,
    budget_tokens: Option<u64>,
    command: &str,
    mut trace: Option<Trace>,
    folded: Vec<Fold>,
) -> String
where
    Vec<T>: HintSource,
{
    let next = hints(command, &items);
    let (items, truncated) = match budget_tokens {
        Some(b) => {
            let json: Vec<String> = items
                .iter()
                .map(|i| serde_json::to_string(i).expect("serialise result"))
                .collect();
            let fit = budget::fit_budget(&json, b);
            set_packing(&mut trace, &json, b);
            let truncated = (fit.omitted > 0).then_some(Truncation {
                shown: fit.kept as u64,
                omitted: fit.omitted as u64,
                budget: b,
            });
            if truncated.is_some() {
                partial.push(budget_partial(b, fit.kept, fit.omitted));
            }
            (items.into_iter().take(fit.kept).collect(), truncated)
        }
        None => (items, None),
    };
    let mut e = envelope(report, truncated, partial, items, next, trace);
    e.folded = folded;
    line(&e)
}

/// Ambiguous candidates answer as a list (the trace rides in the envelope); no match and a
/// stale file have no envelope, so the trace goes to stderr.
fn other_outcome<T>(
    exit: ExitCode,
    report: &Report,
    outcome: Outcome<T>,
    budget_tokens: Option<u64>,
    trace: Option<Trace>,
) -> JsonAnswer {
    match outcome {
        Outcome::Ambiguous(candidates) => {
            let message = format!("ambiguous: {} candidates", candidates.len());
            let stdout = list_envelope::<SymbolHit>(
                report,
                candidates,
                Vec::new(),
                budget_tokens,
                "",
                trace,
                Vec::new(),
            );
            JsonAnswer {
                message: Some(message),
                ..found(exit, report, stdout)
            }
        }
        Outcome::NoMatch(msg) | Outcome::StaleFile(msg) => JsonAnswer {
            message: Some(msg),
            diagnostic: trace
                .as_ref()
                .map(super::diagnostic_text)
                .unwrap_or_default(),
            ..JsonAnswer::new(exit)
        },
        Outcome::Found(_) => JsonAnswer::new(exit),
    }
}

/// A command whose answer is a list; ambiguity answers with the candidates, no match with one
/// message.
pub fn list<T: Serialize>(
    reply: Reply<Vec<T>>,
    budget_tokens: Option<u64>,
    command: &str,
) -> JsonAnswer
where
    Vec<T>: HintSource,
{
    let exit = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(items) => {
            let stdout = list_envelope(
                &reply.report,
                items,
                reply.partial,
                budget_tokens,
                command,
                reply.diagnostic,
                Vec::new(),
            );
            found(exit, &reply.report, stdout)
        }
        other => other_outcome(exit, &reply.report, other, budget_tokens, reply.diagnostic),
    }
}

/// [`list`] for `refs` and `callers`: the envelope also carries the folds of the reply
/// (FEAT-077). Used by the CLI and the MCP tools.
pub fn list_folded<T: Serialize>(
    reply: Reply<Vec<T>>,
    budget_tokens: Option<u64>,
    command: &str,
) -> JsonAnswer
where
    Vec<T>: HintSource,
{
    let exit = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(items) => {
            let stdout = list_envelope(
                &reply.report,
                items,
                reply.partial,
                budget_tokens,
                command,
                reply.diagnostic,
                reply.folded,
            );
            found(exit, &reply.report, stdout)
        }
        other => other_outcome(exit, &reply.report, other, budget_tokens, reply.diagnostic),
    }
}

/// `dangling`: a list that exits 1 when there are any, so CI can gate on it.
pub fn dangling(reply: Reply<Vec<DanglingRef>>, budget_tokens: Option<u64>) -> JsonAnswer {
    let any = matches!(&reply.outcome, Outcome::Found(v) if !v.is_empty());
    let mut reply = reply;
    reply.diagnostic = None;
    let mut a = list(reply, budget_tokens, "");
    if a.exit == ExitCode::Ok && any {
        a.exit = ExitCode::Error;
    }
    a
}

/// `mirrors`: a list that exits 1 when a comparable pair differs. Pairs that cannot be
/// compared are notes and leave the exit code alone.
pub fn mirrors(reply: Reply<Vec<MirrorDrift>>, budget_tokens: Option<u64>) -> JsonAnswer {
    let any = matches!(&reply.outcome, Outcome::Found(v) if v.iter().any(|d| d.comparable));
    let mut reply = reply;
    reply.diagnostic = None;
    let mut a = list(reply, budget_tokens, "");
    if a.exit == ExitCode::Ok && any {
        a.exit = ExitCode::Error;
    }
    a
}

/// `why`: the symbol and its rationale; the budget trims the rationale.
pub fn why(reply: Reply<WhyResult>, budget_tokens: Option<u64>) -> JsonAnswer {
    let exit = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(mut w) => {
            let mut partial = reply.partial;
            let mut truncated = None;
            if let Some(b) = budget_tokens {
                let json: Vec<String> = w
                    .rationale
                    .iter()
                    .map(|r| serde_json::to_string(r).expect("serialise result"))
                    .collect();
                let fit = budget::fit_budget(&json, b);
                if fit.omitted > 0 {
                    truncated = Some(Truncation {
                        shown: fit.kept as u64,
                        omitted: fit.omitted as u64,
                        budget: b,
                    });
                    partial.push(budget_partial(b, fit.kept, fit.omitted));
                    w.rationale.truncate(fit.kept);
                }
            }
            let next = hints("why", &w);
            let e = envelope(&reply.report, truncated, partial, w, next, None);
            found(exit, &reply.report, line(&e))
        }
        other => other_outcome(exit, &reply.report, other, budget_tokens, None),
    }
}

/// `show`: the source lines; the budget trims them.
pub fn show(reply: Reply<ShowResult>, budget_tokens: Option<u64>) -> JsonAnswer {
    let exit = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(mut r) => {
            let next = hints("show", &r);
            let mut partial = reply.partial;
            let mut lines: Vec<String> = r.text.lines().map(str::to_string).collect();
            let mut truncated = None;
            if let Some(b) = budget_tokens {
                let fit = budget::fit_budget(&lines, b);
                if fit.omitted > 0 {
                    truncated = Some(Truncation {
                        shown: fit.kept as u64,
                        omitted: fit.omitted as u64,
                        budget: b,
                    });
                    partial.push(budget_partial(b, fit.kept, fit.omitted));
                    lines.truncate(fit.kept);
                    r.text = lines.join("\n");
                }
            }
            let e = envelope(&reply.report, truncated, partial, r, next, None);
            found(exit, &reply.report, line(&e))
        }
        other => other_outcome(exit, &reply.report, other, budget_tokens, None),
    }
}

/// `explain`: the symbol and its links; the budget trims the links.
pub fn explain(reply: Reply<Explain>, budget_tokens: Option<u64>) -> JsonAnswer {
    let exit = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(mut x) => {
            let next = hints("explain", &x);
            let mut partial = reply.partial;
            let mut truncated = None;
            if let Some(b) = budget_tokens {
                let json: Vec<String> = x
                    .outgoing
                    .iter()
                    .chain(x.incoming.iter())
                    .map(|l| serde_json::to_string(l).expect("serialise result"))
                    .collect();
                let fit = budget::fit_budget(&json, b);
                if fit.omitted > 0 {
                    truncated = Some(Truncation {
                        shown: fit.kept as u64,
                        omitted: fit.omitted as u64,
                        budget: b,
                    });
                    partial.push(budget_partial(b, fit.kept, fit.omitted));
                    let out_kept = fit.kept.min(x.outgoing.len());
                    x.outgoing.truncate(out_kept);
                    x.incoming.truncate(fit.kept - out_kept);
                    if let Some(c) = x.contract.as_mut() {
                        let kept: std::collections::BTreeSet<String> =
                            x.incoming.iter().map(|l| l.other.clone()).collect();
                        c.retain_named(&kept);
                    }
                }
            }
            let e = envelope(&reply.report, truncated, partial, x, next, None);
            found(exit, &reply.report, line(&e))
        }
        other => other_outcome(exit, &reply.report, other, budget_tokens, None),
    }
}

/// `replace`: the range replaced and the range now held.
pub fn replace(reply: Reply<ReplaceResult>, budget_tokens: Option<u64>) -> JsonAnswer {
    let exit = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(r) => {
            let e = envelope(&reply.report, None, Vec::new(), r, Vec::new(), None);
            found(exit, &reply.report, line(&e))
        }
        other => other_outcome(exit, &reply.report, other, budget_tokens, None),
    }
}

/// An insight report: the budget trims whole `items`; the summary stays and the envelope
/// records the truncation. `fails` makes a found report exit 1.
pub fn insight<R>(
    reply: Reply<R>,
    budget_tokens: Option<u64>,
    command: &str,
    fails: fn(&R) -> bool,
) -> JsonAnswer
where
    R: Itemized + Serialize + HintSource,
{
    let exit = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(mut r) => {
            let failing = fails(&r);
            let next = hints(command, &r);
            let mut partial = reply.partial;
            let mut truncated = None;
            if let Some(b) = budget_tokens {
                let json: Vec<String> = r
                    .items_mut()
                    .iter()
                    .map(|i| serde_json::to_string(i).expect("serialise result"))
                    .collect();
                let fit = budget::fit_budget(&json, b);
                if fit.omitted > 0 {
                    truncated = Some(Truncation {
                        shown: fit.kept as u64,
                        omitted: fit.omitted as u64,
                        budget: b,
                    });
                    partial.push(budget_partial(b, fit.kept, fit.omitted));
                    r.items_mut().truncate(fit.kept);
                }
            }
            let e = envelope(&reply.report, truncated, partial, r, next, None);
            let exit = if exit == ExitCode::Ok && failing {
                ExitCode::Error
            } else {
                exit
            };
            found(exit, &reply.report, line(&e))
        }
        other => other_outcome(exit, &reply.report, other, budget_tokens, None),
    }
}

/// `index` and `status`: stdout is `Report::to_json()`; exit Stale when the wait timed out.
pub fn report(r: &Report) -> JsonAnswer {
    JsonAnswer {
        stdout: Some(r.to_json().to_string()),
        warnings: r.warnings.clone(),
        ..JsonAnswer::new(if r.timed_out {
            ExitCode::Stale
        } else {
            ExitCode::Ok
        })
    }
}

/// A [`DistError`]: nothing on stdout, its message, exit Usage for a usage error else Error.
pub fn error(e: &DistError) -> JsonAnswer {
    JsonAnswer {
        message: Some(e.to_string()),
        ..JsonAnswer::new(if e.is_usage() {
            ExitCode::Usage
        } else {
            ExitCode::Error
        })
    }
}

/// Keep whole items of `r` within the budget; returns the truncation record.
pub(super) fn trim_items<R: Itemized>(r: &mut R, budget: Option<u64>) -> Option<Truncation> {
    let b = budget?;
    let json: Vec<String> = r
        .items_mut()
        .iter()
        .map(|i| serde_json::to_string(i).expect("serialise result"))
        .collect();
    let fit = budget::fit_budget(&json, b);
    if fit.omitted == 0 {
        return None;
    }
    r.items_mut().truncate(fit.kept);
    Some(Truncation {
        shown: fit.kept as u64,
        omitted: fit.omitted as u64,
        budget: b,
    })
}

/// An evaluation report: the envelope alone on stdout (no warnings in JSON mode).
pub fn eval_report(
    mut report: EvalReport,
    meta: &Report,
    budget_tokens: Option<u64>,
) -> JsonAnswer {
    let truncated = trim_items(&mut report, budget_tokens);
    let e = envelope(meta, truncated, Vec::new(), report, Vec::new(), None);
    JsonAnswer {
        stdout: Some(line(&e)),
        ..JsonAnswer::new(ExitCode::Ok)
    }
}

/// A sweep report: the envelope alone on stdout.
pub fn sweep_report(
    mut report: SweepReport,
    meta: &Report,
    budget_tokens: Option<u64>,
) -> JsonAnswer {
    let truncated = trim_items(&mut report, budget_tokens);
    let e = envelope(meta, truncated, Vec::new(), report, Vec::new(), None);
    JsonAnswer {
        stdout: Some(line(&e)),
        ..JsonAnswer::new(ExitCode::Ok)
    }
}

/// The `profiles list` JSON line.
pub fn profile_listing(l: &katalogr_dist::ProfileListing) -> String {
    use serde_json::json;
    let project: Vec<_> = l
        .project
        .iter()
        .map(|p| match &p.replaces {
            Some(r) => json!({ "file": p.file, "replaces": r }),
            None => json!({ "file": p.file }),
        })
        .collect();
    let refused: Vec<_> = l
        .refused
        .iter()
        .map(|e| json!({ "file": e.file, "line": e.line, "message": e.message }))
        .collect();
    json!({ "builtins": l.builtins, "project": project, "refused": refused }).to_string()
}

/// The `profiles show` JSON line.
pub fn profile_text(name: &str, text: &str) -> String {
    serde_json::json!({ "name": name, "text": text }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(outcome: Outcome<()>, timed_out: bool) -> Reply<()> {
        Reply {
            outcome,
            report: Report {
                watermark: 1,
                pending: u64::from(timed_out),
                stale: timed_out,
                timed_out,
                queued: 0,
                warnings: Vec::new(),
            },
            diagnostic: None,
            partial: Vec::new(),
            folded: Vec::new(),
        }
    }

    #[test]
    fn timed_out_no_match_exits_stale() {
        let r = reply(Outcome::NoMatch("none".into()), true);
        assert_eq!(exit_for(&r), ExitCode::Stale);
        let r = reply(Outcome::NoMatch("none".into()), false);
        assert_eq!(exit_for(&r), ExitCode::NoMatch);
    }

    #[test]
    fn timed_out_ambiguous_exits_stale() {
        let r = reply(Outcome::Ambiguous(Vec::new()), true);
        assert_eq!(exit_for(&r), ExitCode::Stale);
        let r = reply(Outcome::Ambiguous(Vec::new()), false);
        assert_eq!(exit_for(&r), ExitCode::Ambiguous);
    }
}
