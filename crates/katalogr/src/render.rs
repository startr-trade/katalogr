//! Text and JSON output of query results, and their exit codes.
//!
//! Text is for people and carries no compatibility promise: one line per hit,
//! `kind  name  path:line  [confidence]`. JSON is the versioned envelope.

use katalogr_dist::query::{
    CallerHit, Envelope, MapEntry, RESULT_SCHEMA_VERSION, RefHit, Serialize, SymbolHit, Truncation,
    WhyResult, budget,
};
use katalogr_dist::{Outcome, Reply, Report};

use crate::cli::Cli;
use crate::exit::ExitCode;

fn place(h: &SymbolHit) -> String {
    format!("{}:{}", h.path, h.span.start_line)
}

pub fn symbol_line(h: &SymbolHit) -> String {
    format!("{}  {}  {}", h.kind, h.name, place(h))
}

pub fn ref_line(r: &RefHit) -> String {
    let mut s = format!(
        "{}  {}  {}  [{:?}]",
        r.kind,
        r.from.name,
        place(&r.from),
        r.confidence
    );
    if !r.via.is_empty() {
        s.push_str(&format!("  via {}", r.via.join(" > ")));
    }
    s
}

pub fn caller_line(c: &CallerHit) -> String {
    format!(
        "{}  {}  {}  [{:?}]  depth {}",
        c.caller.kind,
        c.caller.name,
        place(&c.caller),
        c.confidence,
        c.depth
    )
}

pub fn map_line(m: &MapEntry) -> String {
    format!("{:>4}  {}", m.in_degree, symbol_line(&m.symbol))
}

fn envelope<T>(report: &Report, truncated: Option<Truncation>, result: T) -> Envelope<T> {
    Envelope {
        schema: RESULT_SCHEMA_VERSION,
        watermark: report.watermark,
        pending: report.pending,
        stale: report.stale,
        truncated,
        result,
    }
}

fn note_stale(report: &Report) {
    for w in &report.warnings {
        eprintln!("katalogr: warning: {w}");
    }
    if report.stale {
        eprintln!(
            "katalogr: warning: answer may be stale ({} change(s) pending)",
            report.pending
        );
    }
}

/// Print `items` as text lines or a JSON envelope, within the budget.
fn print_list<T: Serialize>(
    cli: &Cli,
    report: &Report,
    items: Vec<T>,
    line: impl Fn(&T) -> String,
) {
    note_stale(report);
    if cli.json {
        let (items, truncated) = match cli.budget {
            Some(b) => {
                let json: Vec<String> = items
                    .iter()
                    .map(|i| serde_json::to_string(i).expect("serialise result"))
                    .collect();
                let fit = budget::fit_budget(&json, b);
                let truncated = (fit.omitted > 0).then_some(Truncation {
                    shown: fit.kept as u64,
                    omitted: fit.omitted as u64,
                    budget: b,
                });
                (items.into_iter().take(fit.kept).collect(), truncated)
            }
            None => (items, None),
        };
        let e = envelope(report, truncated, items);
        println!("{}", serde_json::to_string(&e).expect("serialise envelope"));
    } else {
        print_lines(cli, items.iter().map(line).collect());
    }
}

fn print_lines(cli: &Cli, lines: Vec<String>) {
    let (kept, omitted) = match cli.budget {
        Some(b) => {
            let fit = budget::fit_budget(&lines, b);
            (fit.kept, fit.omitted)
        }
        None => (lines.len(), 0),
    };
    for l in &lines[..kept] {
        println!("{l}");
    }
    if omitted > 0 {
        println!("{}", budget::omitted_notice(omitted));
    }
}

fn exit_for<T>(reply: &Reply<T>) -> ExitCode {
    match &reply.outcome {
        Outcome::Ambiguous(_) => ExitCode::Ambiguous,
        Outcome::NoMatch(_) => ExitCode::NoMatch,
        Outcome::Found(_) if reply.report.timed_out => ExitCode::Stale,
        Outcome::Found(_) => ExitCode::Ok,
    }
}

/// Render a command whose answer is a list; ambiguity prints the candidates and exits 3,
/// no match prints one line on stderr and exits 4.
pub fn list<T: Serialize>(cli: &Cli, reply: Reply<Vec<T>>, line: fn(&T) -> String) -> ExitCode {
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(items) => print_list(cli, &reply.report, items, line),
        other => other_outcomes(cli, &reply.report, other),
    }
    code
}

/// Render `why`: the symbol, then its rationale.
pub fn why(cli: &Cli, reply: Reply<WhyResult>) -> ExitCode {
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(mut w) => {
            note_stale(&reply.report);
            if cli.json {
                let mut truncated = None;
                if let Some(b) = cli.budget {
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
                        w.rationale.truncate(fit.kept);
                    }
                }
                let e = envelope(&reply.report, truncated, w);
                println!("{}", serde_json::to_string(&e).expect("serialise envelope"));
            } else {
                let mut lines = vec![symbol_line(&w.symbol)];
                lines.extend(
                    w.rationale
                        .iter()
                        .map(|r| format!("  {}  {}  {}", r.kind, r.label, r.target)),
                );
                print_lines(cli, lines);
            }
        }
        other => other_outcomes(cli, &reply.report, other),
    }
    code
}

fn other_outcomes<T>(cli: &Cli, report: &Report, outcome: Outcome<T>) {
    match outcome {
        Outcome::Ambiguous(candidates) => {
            eprintln!("katalogr: ambiguous: {} candidates", candidates.len());
            print_list(cli, report, candidates, symbol_line);
        }
        Outcome::NoMatch(msg) => eprintln!("katalogr: {msg}"),
        Outcome::Found(_) => {}
    }
}
