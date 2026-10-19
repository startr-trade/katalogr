//! Text and JSON output of query results, and their exit codes.
//!
//! Text is for people and carries no compatibility promise: one line per hit,
//! `kind  name  path:line  [confidence]`. JSON is the versioned envelope.

use katalogr_dist::analytics::{
    Community, CommunityReport, Cycle, CycleReport, DeadCandidate, DeadCodeReport, HubEntry,
    HubReport, Itemized, RationaleEntry, RationaleReport,
};
use katalogr_dist::query::{
    CallerHit, DanglingRef, Direction, Envelope, Explain, FieldShape, HintSource, MapEntry,
    MirrorDrift, PathStep, RESULT_SCHEMA_VERSION, RefHit, ReplaceResult, SearchHit, Serialize,
    ShowResult, SymbolHit, Truncation, WhyResult, budget, hints,
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

fn shape_text(s: Option<&FieldShape>) -> String {
    let Some(s) = s else {
        return "-".to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    parts.extend(s.field_type.clone());
    parts.extend(
        s.required
            .map(|r| if r { "required" } else { "optional" }.to_string()),
    );
    parts.extend(
        s.many
            .map(|m| if m { "many" } else { "single" }.to_string()),
    );
    if parts.is_empty() {
        "?".to_string()
    } else {
        parts.join(" ")
    }
}

/// One block per drift: the contract, `left -> right`, then one line per difference. A pair
/// that cannot be compared is a note.
pub fn mirror_block(d: &MirrorDrift) -> String {
    let mut out = format!("{}\n  {} -> {}", d.contract, d.left.name, d.right.name);
    if !d.comparable {
        out.push_str("\n  note: not compared, a side has no structural fields");
    }
    for f in &d.differences {
        out.push_str(&format!(
            "\n  {}  {}  left: {}  right: {}",
            f.what,
            f.field,
            shape_text(f.left.as_ref()),
            shape_text(f.right.as_ref())
        ));
    }
    out
}

/// Render `mirrors`: exit 1 when a comparable pair differs, so CI can gate on it. Pairs that
/// cannot be compared are notes and leave the exit code alone.
pub fn mirrors(cli: &Cli, reply: Reply<Vec<MirrorDrift>>) -> ExitCode {
    let code = exit_for(&reply);
    let any = matches!(&reply.outcome, Outcome::Found(v) if v.iter().any(|d| d.comparable));
    match reply.outcome {
        Outcome::Found(items) => print_list(cli, &reply.report, items, mirror_block, ""),
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
                lines.extend(
                    w.notes
                        .iter()
                        .map(|n| format!("  {}  line {}  {}", n.marker, n.line, n.text)),
                );
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

/// How one insight report prints as text, and when it fails the command.
pub struct Shape<R: Itemized> {
    /// Summary lines, always shown.
    pub header: fn(&R) -> Vec<String>,
    /// One line per item.
    pub line: fn(&R::Item) -> String,
    /// Lines after the items.
    pub footer: fn(&R) -> Vec<String>,
    /// True when the report makes the command exit 1.
    pub fails: fn(&R) -> bool,
}

fn no_lines<R>(_: &R) -> Vec<String> {
    Vec::new()
}

fn never<R>(_: &R) -> bool {
    false
}

fn level_name(l: katalogr_dist::analytics::Level) -> &'static str {
    use katalogr_dist::analytics::Level;
    match l {
        Level::Symbol => "symbol",
        Level::File => "file",
        Level::Package => "package",
    }
}

fn node_place(n: &katalogr_dist::analytics::NodeHit) -> String {
    match (&n.path, n.line) {
        (Some(p), Some(l)) => format!("  {p}:{l}"),
        (Some(p), None) => format!("  {p}"),
        _ => String::new(),
    }
}

pub const HUBS: Shape<HubReport> = Shape {
    header: |r| {
        vec![format!(
            "{} level: {} nodes, mean degree {:.2}",
            level_name(r.level),
            r.nodes,
            r.mean_degree
        )]
    },
    line: |h: &HubEntry| {
        format!(
            "{:>8.2}  in {:.2}  out {:.2}  {}  {}{}",
            h.degree,
            h.in_degree,
            h.out_degree,
            h.node.kind,
            h.node.name,
            node_place(&h.node)
        )
    },
    footer: no_lines,
    fails: never,
};

pub const COMMUNITIES: Shape<CommunityReport> = Shape {
    header: |r| {
        let mut v = vec![format!(
            "modularity {:.4}  {} communities  {} unclustered",
            r.modularity, r.communities_total, r.unclustered
        )];
        v.extend(r.note.iter().cloned());
        v
    },
    line: |c: &Community| {
        let top: Vec<&str> = c.top.iter().map(|n| n.name.as_str()).collect();
        format!(
            "#{}  {} members  cohesion {:.3}  {}  top: {}",
            c.id,
            c.size,
            c.cohesion,
            c.dir.as_deref().unwrap_or("-"),
            top.join(", ")
        )
    },
    footer: no_lines,
    fails: never,
};

pub const CYCLES: Shape<CycleReport> = Shape {
    header: |r| {
        let mut v = vec![format!(
            "{} cycles, {} {} level nodes in cycles",
            r.cycles_total,
            r.cyclic_nodes,
            level_name(r.level)
        )];
        if r.skipped_contracts > 0 {
            v.push(format!(
                "{} contracts skipped: too many consumers and providers",
                r.skipped_contracts
            ));
        }
        v
    },
    line: |c: &Cycle| {
        let mut path = String::new();
        for (i, s) in c.witness.iter().enumerate() {
            if i == 0 {
                path.push_str(&s.from);
            }
            path.push_str(&format!(" -{}-> {}", s.kind, s.to));
        }
        format!("#{}  size {}  [{:?}]  {path}", c.id, c.size, c.confidence)
    },
    footer: no_lines,
    fails: |r| r.cycles_total > 0,
};

fn dead_header(r: &DeadCodeReport) -> Vec<String> {
    let mut v = vec![format!(
        "{} entry points; {} high, {} medium, {} low",
        r.roots, r.high, r.medium, r.low
    )];
    v.extend(r.note.iter().cloned());
    v
}

fn dead_line(c: &DeadCandidate) -> String {
    format!(
        "{}  {}  {}  {}  {}:{}  in {}",
        kebab(&format!("{:?}", c.confidence)),
        kebab(&format!("{:?}", c.class)),
        c.symbol.kind,
        c.symbol.name,
        c.symbol.path,
        c.symbol.span.start_line,
        c.incoming
    )
}

/// `WeaklyReached` as `weakly-reached`.
fn kebab(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('-');
        }
        out.extend(c.to_lowercase());
    }
    out
}

pub const DEAD_CODE: Shape<DeadCodeReport> = Shape {
    header: dead_header,
    line: dead_line,
    footer: no_lines,
    fails: never,
};

/// With `--check`, any listed candidate fails the command.
pub const DEAD_CODE_CHECK: Shape<DeadCodeReport> = Shape {
    header: dead_header,
    line: dead_line,
    footer: no_lines,
    fails: |r| !r.items.is_empty(),
};

pub const RATIONALE: Shape<RationaleReport> = Shape {
    header: no_lines,
    line: |e: &RationaleEntry| {
        format!(
            "{}  {}  {}:{}  {}",
            e.kind, e.node.name, e.node.path, e.line, e.text
        )
    },
    footer: |r| {
        r.uncovered
            .iter()
            .map(|h| format!("no rationale: {}", h.name))
            .collect()
    },
    fails: never,
};

/// Render an insight report as text or a JSON envelope. The budget trims whole `items`; the
/// summary stays and the envelope records the truncation.
pub fn insight<R>(cli: &Cli, reply: Reply<R>, command: &str, shape: &Shape<R>) -> ExitCode
where
    R: Itemized + Serialize + HintSource,
{
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(mut r) => {
            note_stale(&reply.report);
            let failing = (shape.fails)(&r);
            let next = hints(command, &r);
            let lines: Vec<String> = r.items_mut().iter().map(|i| (shape.line)(i)).collect();
            if cli.json {
                let mut truncated = None;
                if let Some(b) = cli.budget {
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
                        r.items_mut().truncate(fit.kept);
                    }
                }
                let e = envelope(&reply.report, truncated, r, next);
                println!("{}", serde_json::to_string(&e).expect("serialise envelope"));
            } else {
                for l in (shape.header)(&r) {
                    println!("{l}");
                }
                print_lines(cli, lines);
                for l in (shape.footer)(&r) {
                    println!("{l}");
                }
                print_next(&next);
            }
            if code == ExitCode::Ok && failing {
                return ExitCode::Error;
            }
        }
        other => other_outcomes(cli, &reply.report, other),
    }
    code
}
