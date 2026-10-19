//! Text and JSON output of query results, and their exit codes.
//!
//! Text is for people and carries no compatibility promise: one line per hit,
//! `kind  name  path:line  [confidence]`. JSON is the versioned envelope.

use katalogr_dist::query::{
    CallerHit, DanglingRef, Direction, Envelope, Explain, HintSource, MapEntry, PathStep,
    RESULT_SCHEMA_VERSION, RefHit, ReplaceResult, SearchHit, Serialize, ShowResult, SymbolHit,
    Truncation, WhyResult, budget, hints,
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
    let via = c
        .via
        .as_ref()
        .map(|v| format!("  via {v}"))
        .unwrap_or_default();
    format!(
        "{}  {}  {}  [{:?}]  depth {}{via}",
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

pub fn search_line(h: &SearchHit) -> String {
    let matched: Vec<String> = h
        .matched
        .iter()
        .map(|m| {
            let star = if m.prefix { "*" } else { "" };
            format!("{}{star}@{}", m.term, m.field)
        })
        .collect();
    format!(
        "{:.6}  {}  [{}]",
        h.score,
        symbol_line(&h.symbol),
        matched.join(" ")
    )
}

pub fn path_line(s: &PathStep) -> String {
    let link = match s.direction {
        Direction::Forward => format!("-{}->", s.kind),
        Direction::Backward => format!("<-{}-", s.kind),
    };
    format!("{} {} {}  [{:?}]", s.from, link, s.to, s.confidence)
}

fn envelope<T>(
    report: &Report,
    truncated: Option<Truncation>,
    result: T,
    next: Vec<String>,
) -> Envelope<T> {
    Envelope {
        schema: RESULT_SCHEMA_VERSION,
        watermark: report.watermark,
        pending: report.pending,
        stale: report.stale,
        truncated,
        result,
        next,
    }
}

/// The `next:` footer of text output.
fn print_next(next: &[String]) {
    if !next.is_empty() {
        println!("next: {}", next.join("; "));
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

/// Print `items` as text lines or a JSON envelope, within the budget, with the hints for
/// `command` (none when `command` is empty).
fn print_list<T: Serialize>(
    cli: &Cli,
    report: &Report,
    items: Vec<T>,
    line: impl Fn(&T) -> String,
    command: &str,
) where
    Vec<T>: HintSource,
{
    note_stale(report);
    let next = hints(command, &items);
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
        let e = envelope(report, truncated, items, next);
        println!("{}", serde_json::to_string(&e).expect("serialise envelope"));
    } else {
        print_lines(cli, items.iter().map(line).collect());
        print_next(&next);
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
        Outcome::StaleFile(_) => ExitCode::Stale,
        Outcome::Found(_) if reply.report.timed_out => ExitCode::Stale,
        Outcome::Found(_) => ExitCode::Ok,
    }
}

/// Render a command whose answer is a list; ambiguity prints the candidates and exits 3,
/// no match prints one line on stderr and exits 4.
pub fn list<T: Serialize>(
    cli: &Cli,
    reply: Reply<Vec<T>>,
    line: fn(&T) -> String,
    command: &str,
) -> ExitCode
where
    Vec<T>: HintSource,
{
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(items) => print_list(cli, &reply.report, items, line, command),
        other => other_outcomes(cli, &reply.report, other),
    }
    code
}

pub fn dangling_line(d: &DanglingRef) -> String {
    format!(
        "{}  {}  {}  wants {}",
        d.kind,
        d.from.name,
        place(&d.from),
        d.wanted.join(" | ")
    )
}

/// Render `dangling`: one line per unresolved reference; exit 1 when there are any, so CI can
/// gate on it.
pub fn dangling(cli: &Cli, reply: Reply<Vec<DanglingRef>>) -> ExitCode {
    let code = exit_for(&reply);
    let any = matches!(&reply.outcome, Outcome::Found(v) if !v.is_empty());
    match reply.outcome {
        Outcome::Found(items) => print_list(cli, &reply.report, items, dangling_line, ""),
        other => other_outcomes(cli, &reply.report, other),
    }
    if code == ExitCode::Ok && any {
        ExitCode::Error
    } else {
        code
    }
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
                let next = hints("why", &w);
                let e = envelope(&reply.report, truncated, w, next);
                println!("{}", serde_json::to_string(&e).expect("serialise envelope"));
            } else {
                let mut lines = vec![symbol_line(&w.symbol)];
                lines.extend(
                    w.rationale
                        .iter()
                        .chain(w.goals.iter())
                        .map(|r| format!("  {}  {}  {}", r.kind, r.label, r.target)),
                );
                lines.extend(w.commits.iter().map(|c| {
                    format!("  commit  {}  {}", &c.sha[..c.sha.len().min(12)], c.summary)
                }));
                if let Some(n) = &w.note {
                    lines.push(format!("  note  {n}"));
                }
                print_lines(cli, lines);
                print_next(&hints("why", &w));
            }
        }
        other => other_outcomes(cli, &reply.report, other),
    }
    code
}

/// Render `show`: a `path:start-end` header, then the text.
pub fn show(cli: &Cli, reply: Reply<ShowResult>) -> ExitCode {
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(mut r) => {
            note_stale(&reply.report);
            let next = hints("show", &r);
            let mut lines: Vec<String> = r.text.lines().map(str::to_string).collect();
            if cli.json {
                let mut truncated = None;
                if let Some(b) = cli.budget {
                    let fit = budget::fit_budget(&lines, b);
                    if fit.omitted > 0 {
                        truncated = Some(Truncation {
                            shown: fit.kept as u64,
                            omitted: fit.omitted as u64,
                            budget: b,
                        });
                        lines.truncate(fit.kept);
                        r.text = lines.join("\n");
                    }
                }
                let e = envelope(&reply.report, truncated, r, next);
                println!("{}", serde_json::to_string(&e).expect("serialise envelope"));
            } else {
                lines.insert(0, format!("{}:{}-{}", r.file, r.lines.0, r.lines.1));
                print_lines(cli, lines);
                print_next(&next);
            }
        }
        other => other_outcomes(cli, &reply.report, other),
    }
    code
}

/// Render `explain`: the symbol, its signature and doc, then its links.
pub fn explain(cli: &Cli, reply: Reply<Explain>) -> ExitCode {
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(mut x) => {
            note_stale(&reply.report);
            let next = hints("explain", &x);
            if cli.json {
                let mut truncated = None;
                if let Some(b) = cli.budget {
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
                        let out_kept = fit.kept.min(x.outgoing.len());
                        x.outgoing.truncate(out_kept);
                        x.incoming.truncate(fit.kept - out_kept);
                    }
                }
                let e = envelope(&reply.report, truncated, x, next);
                println!("{}", serde_json::to_string(&e).expect("serialise envelope"));
            } else {
                let mut lines = vec![symbol_line(&x.symbol)];
                if let Some(s) = &x.signature {
                    lines.push(format!("signature: {s}"));
                }
                if let Some(d) = &x.doc {
                    lines.push(format!("doc: {d}"));
                }
                for (title, links, total) in [
                    ("outgoing", &x.outgoing, x.outgoing_total),
                    ("incoming", &x.incoming, x.incoming_total),
                ] {
                    if links.is_empty() {
                        continue;
                    }
                    if total as usize > links.len() {
                        lines.push(format!("{title} (first {} of {total}):", links.len()));
                    } else {
                        lines.push(format!("{title} ({total}):"));
                    }
                    lines.extend(links.iter().map(|l| {
                        let origin = l
                            .origin
                            .as_ref()
                            .map(|o| format!("  origin {o}"))
                            .unwrap_or_default();
                        format!("  {}  {}  [{:?}]{origin}", l.kind, l.other, l.confidence)
                    }));
                }
                if !x.items.is_empty() {
                    lines.push(format!("items: {}", x.items.join(", ")));
                }
                print_lines(cli, lines);
                print_next(&next);
            }
        }
        other => other_outcomes(cli, &reply.report, other),
    }
    code
}

/// Render `replace`: the range replaced and the range now held.
pub fn replace(cli: &Cli, reply: Reply<ReplaceResult>) -> ExitCode {
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(r) => {
            note_stale(&reply.report);
            if cli.json {
                let e = envelope(&reply.report, None, r, Vec::new());
                println!("{}", serde_json::to_string(&e).expect("serialise envelope"));
            } else {
                println!(
                    "replaced {}:{}-{} with {}:{}-{}",
                    r.file, r.replaced.0, r.replaced.1, r.file, r.lines.0, r.lines.1
                );
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
            print_list(cli, report, candidates, symbol_line, "");
        }
        Outcome::NoMatch(msg) | Outcome::StaleFile(msg) => eprintln!("katalogr: {msg}"),
        Outcome::Found(_) => {}
    }
}

/// The `doctor` report: JSON with `--json`, else one line per finding with its fix.
pub fn doctor(cli: &Cli, r: &katalogr_dist::doctor::Report) -> ExitCode {
    if cli.json {
        println!("{}", serde_json::to_string(r).expect("serialise report"));
    } else {
        print!("{}", doctor_text(r));
    }
    if r.exit_failing() {
        ExitCode::Error
    } else {
        ExitCode::Ok
    }
}

/// The text form of a doctor report.
pub fn doctor_text(r: &katalogr_dist::doctor::Report) -> String {
    use katalogr_dist::doctor::CheckStatus;
    use std::fmt::Write;
    let v = &r.versions;
    let mut out = format!(
        "katalogr doctor: katalogr {}, shard format {}",
        v.katalogr, v.shard_format
    );
    if let Some(k) = v.kindex_shard_format.filter(|k| *k != v.shard_format) {
        let _ = write!(out, " (kindex {k})");
    }
    let _ = write!(out, ", cache schema {}", v.cache_schema);
    if let Some(s) = v.stored_cache_schema.filter(|s| *s != v.cache_schema) {
        let _ = write!(out, " (stored {s})");
    }
    out.push('\n');
    for f in &r.findings {
        let _ = writeln!(out, "{:<7} {}  {}", f.severity.as_str(), f.code, f.message);
        if let Some(fix) = &f.fix {
            let _ = writeln!(out, "        fix: {fix}");
        }
        if !f.sample.is_empty() {
            let _ = writeln!(out, "        keys: {}", f.sample.join(", "));
        }
    }
    let n = r.checks.len();
    if r.findings.is_empty() {
        let _ = writeln!(out, "summary: healthy; {n} checks");
    } else {
        let with = r
            .checks
            .iter()
            .filter(|c| c.status == CheckStatus::Findings)
            .count();
        let skipped = r
            .checks
            .iter()
            .filter(|c| c.status == CheckStatus::Skipped)
            .count();
        let _ = writeln!(
            out,
            "summary: {} error, {} warning, {} info; {n} checks ({with} with findings, {skipped} skipped)",
            r.summary.error, r.summary.warning, r.summary.info
        );
    }
    let mut next: Vec<&str> = Vec::new();
    for f in r.findings.iter().take(3) {
        if let Some(fix) = f.fix.as_deref()
            && (fix.starts_with("katalogr ") || fix.starts_with("git "))
            && !next.contains(&fix)
        {
            next.push(fix);
        }
    }
    if !next.is_empty() {
        let _ = writeln!(out, "next: {}", next.join("; "));
    }
    out
}
