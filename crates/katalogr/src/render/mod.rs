//! Text and JSON output of query results, and their exit codes.
//!
//! Text is for people and carries no compatibility promise: one line per hit,
//! `kind  name  path:line  [confidence]`. JSON is the versioned envelope, built by [`json`]
//! (shared with the MCP server) and printed by [`emit`].

pub mod json;

use katalogr_dist::analytics::{
    Community, CommunityReport, CoverageCounts, CoverageReport, CoverageRow, Cycle, CycleReport,
    DeadCandidate, DeadCodeReport, HubEntry, HubReport, Itemized, RationaleEntry, RationaleReport,
};
use katalogr_dist::query::{
    CallerHit, DanglingRef, Direction, Explain, FieldShape, HintSource, MapEntry, MirrorDrift,
    Partial, PathStep, RefHit, ReplaceResult, SearchHit, Serialize, ShowResult, SymbolHit, Trace,
    WhyResult, budget, hints,
};
use katalogr_dist::{Outcome, Reply, Report};

use crate::cli::Cli;
use crate::exit::ExitCode;

use self::json::{JsonAnswer, budget_partial, exit_for, set_packing, stale_warnings};

/// Print a [`JsonAnswer`] as the CLI does: warnings to stderr, the stdout line, then the
/// message (and any diagnostic lines) to stderr.
pub fn emit(a: &JsonAnswer) -> ExitCode {
    for w in &a.warnings {
        eprintln!("katalogr: warning: {w}");
    }
    if let Some(out) = &a.stdout {
        println!("{out}");
    }
    if let Some(m) = &a.message {
        eprintln!("katalogr: {m}");
    }
    for l in &a.diagnostic {
        eprintln!("{l}");
    }
    a.exit
}

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

/// The `partial:` lines of text output, after the items and before `next:`.
fn print_partial(partial: &[Partial]) {
    for p in partial {
        println!("{}", p.line());
    }
}

/// The `next:` footer of text output.
fn print_next(next: &[String]) {
    if !next.is_empty() {
        println!("next: {}", next.join("; "));
    }
}

fn note_stale(report: &Report) {
    for w in stale_warnings(report) {
        eprintln!("katalogr: warning: {w}");
    }
}

/// Print `items` as text lines within the budget, with the hints for `command` (none when
/// `command` is empty). JSON goes through [`json`].
fn print_list<T: Serialize>(
    cli: &Cli,
    report: &Report,
    items: Vec<T>,
    mut partial: Vec<Partial>,
    line: impl Fn(&T) -> String,
    command: &str,
    mut trace: Option<Trace>,
) where
    Vec<T>: HintSource,
{
    note_stale(report);
    let next = hints(command, &items);
    let lines: Vec<String> = items.iter().map(line).collect();
    if let Some(b) = cli.budget {
        set_packing(&mut trace, &lines, b);
    }
    print_lines(cli, lines, &mut partial);
    print_partial(&partial);
    print_next(&next);
    print_trace(trace.as_ref());
}

/// The trace as `diagnostic:` lines on stderr.
fn print_trace(trace: Option<&Trace>) {
    if let Some(t) = trace {
        for l in diagnostic_text(t) {
            eprintln!("{l}");
        }
    }
}

fn names(v: &[String]) -> String {
    if v.is_empty() {
        "-".to_string()
    } else {
        v.join(",")
    }
}

/// One `diagnostic:` line per item of the trace; floats have six decimals.
pub fn diagnostic_text(t: &Trace) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(r) = &t.routing {
        out.push(match r.command.as_str() {
            "search" => format!(
                "diagnostic: route search tokens={} dropped={}",
                names(&r.tokens),
                names(&r.dropped)
            ),
            "refs" => format!(
                "diagnostic: route refs input={:?} kind={} tier={} candidates={} after_collapse={}",
                r.input,
                r.kind,
                r.tier.as_deref().unwrap_or("-"),
                r.candidates,
                r.after_collapse
            ),
            c => format!("diagnostic: route {c}"),
        });
    }
    for x in &t.terms {
        let p = &x.prefix;
        let prefix = if !p.attempted {
            "not attempted".to_string()
        } else {
            let listed: Vec<String> = p
                .terms
                .iter()
                .map(|e| format!("{}({})", e.term, e.postings))
                .collect();
            format!(
                "{} expanded (cap {}): {}",
                p.expanded,
                if p.cap_hit { "hit" } else { "not hit" },
                listed.join(" ")
            )
        };
        out.push(format!(
            "diagnostic: term {} exact_postings={} matching_docs={} idf={:.6} prefix={}",
            x.token, x.exact_postings, x.matching_docs, x.idf, prefix
        ));
    }
    for r in &t.ranking {
        let parts: Vec<String> = r
            .parts
            .iter()
            .map(|p| {
                format!(
                    "{}[{}:{} t={:.6} c={:.6}]",
                    p.token, p.via, p.term, p.t, p.contribution
                )
            })
            .collect();
        out.push(format!(
            "diagnostic: rank {} {} score={:.6}{} = {}",
            r.position,
            r.symbol,
            r.score,
            if r.tied_with_next {
                " tied-with-next"
            } else {
                ""
            },
            parts.join(" + ")
        ));
    }
    if let Some(s) = &t.rank_source {
        out.push(format!(
            "diagnostic: rank-source {} watermark={} rank_watermark={} damping={:.6} iterations={}",
            s.source,
            s.watermark,
            s.rank_watermark.map_or("-".to_string(), |w| w.to_string()),
            s.damping,
            s.iterations
        ));
    }
    for w in &t.weights {
        out.push(format!(
            "diagnostic: weight {} <- {} kind={} confidence={} fanout={} weight={:.6}",
            w.to,
            w.from,
            w.kind,
            w.confidence,
            w.fanout.map_or("-".to_string(), |f| f.to_string()),
            w.weight
        ));
    }
    if let Some(p) = &t.packing {
        out.push(format!(
            "diagnostic: pack budget={} estimator={} considered={} kept={} omitted={} used={} next-needed={}",
            p.budget,
            p.estimator,
            p.considered,
            p.kept,
            p.omitted,
            p.used_tokens,
            p.first_omitted_tokens.map_or("-".to_string(), |n| n.to_string())
        ));
        let entries: Vec<String> = p
            .entries
            .iter()
            .map(|e| {
                let mut s = format!("{}:{}", e.index, e.tokens);
                if !e.kept {
                    s.push_str("(omitted)");
                }
                if let Some(tier) = &e.tier {
                    s.push_str(&format!(" tier={tier}"));
                }
                if let Some(reason) = &e.reason {
                    s.push_str(&format!(" reason={reason}"));
                }
                s
            })
            .collect();
        out.push(format!("diagnostic: pack-entries {}", entries.join(" ")));
    }
    if !t.omitted.is_empty() {
        let cut: Vec<String> = t.omitted.iter().map(|(k, n)| format!("{k}={n}")).collect();
        out.push(format!("diagnostic: omitted {}", cut.join(" ")));
    }
    out
}

/// Print `lines` within the budget; a budget cut adds its marker to `partial`.
fn print_lines(cli: &Cli, lines: Vec<String>, partial: &mut Vec<Partial>) {
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
        if let Some(b) = cli.budget {
            partial.push(budget_partial(b, kept, omitted));
        }
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
    if cli.json {
        return emit(&json::list(reply, cli.budget, command));
    }
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(items) => {
            print_list(
                cli,
                &reply.report,
                items,
                reply.partial,
                line,
                command,
                reply.diagnostic,
            );
        }
        other => other_outcomes(cli, &reply.report, other, reply.diagnostic),
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
    if cli.json {
        return emit(&json::dangling(reply, cli.budget));
    }
    let code = exit_for(&reply);
    let any = matches!(&reply.outcome, Outcome::Found(v) if !v.is_empty());
    match reply.outcome {
        Outcome::Found(items) => print_list(
            cli,
            &reply.report,
            items,
            reply.partial,
            dangling_line,
            "",
            None,
        ),
        other => other_outcomes(cli, &reply.report, other, None),
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
    if cli.json {
        return emit(&json::mirrors(reply, cli.budget));
    }
    let code = exit_for(&reply);
    let any = matches!(&reply.outcome, Outcome::Found(v) if v.iter().any(|d| d.comparable));
    match reply.outcome {
        Outcome::Found(items) => print_list(
            cli,
            &reply.report,
            items,
            reply.partial,
            mirror_block,
            "",
            None,
        ),
        other => other_outcomes(cli, &reply.report, other, None),
    }
    if code == ExitCode::Ok && any {
        ExitCode::Error
    } else {
        code
    }
}

/// Render `why`: the symbol, then its rationale.
pub fn why(cli: &Cli, reply: Reply<WhyResult>) -> ExitCode {
    if cli.json {
        return emit(&json::why(reply, cli.budget));
    }
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(w) => {
            note_stale(&reply.report);
            let mut partial = reply.partial;
            let mut lines = vec![symbol_line(&w.symbol)];
            lines.extend(
                w.rationale
                    .iter()
                    .chain(w.goals.iter())
                    .map(|r| format!("  {}  {}  {}", r.kind, r.label, r.target)),
            );
            lines.extend(
                w.commits
                    .iter()
                    .map(|c| format!("  commit  {}  {}", &c.sha[..c.sha.len().min(12)], c.summary)),
            );
            lines.extend(
                w.notes
                    .iter()
                    .map(|n| format!("  {}  line {}  {}", n.marker, n.line, n.text)),
            );
            if let Some(n) = &w.note {
                lines.push(format!("  note  {n}"));
            }
            print_lines(cli, lines, &mut partial);
            print_partial(&partial);
            print_next(&hints("why", &w));
        }
        other => other_outcomes(cli, &reply.report, other, None),
    }
    code
}

/// Render `show`: a `path:start-end` header, then the text.
pub fn show(cli: &Cli, reply: Reply<ShowResult>) -> ExitCode {
    if cli.json {
        return emit(&json::show(reply, cli.budget));
    }
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(r) => {
            note_stale(&reply.report);
            let next = hints("show", &r);
            let mut partial = reply.partial;
            let mut lines: Vec<String> = r.text.lines().map(str::to_string).collect();
            lines.insert(0, format!("{}:{}-{}", r.file, r.lines.0, r.lines.1));
            print_lines(cli, lines, &mut partial);
            print_partial(&partial);
            print_next(&next);
        }
        other => other_outcomes(cli, &reply.report, other, None),
    }
    code
}

/// Render `explain`: the symbol, its signature and doc, then its links.
pub fn explain(cli: &Cli, reply: Reply<Explain>) -> ExitCode {
    if cli.json {
        return emit(&json::explain(reply, cli.budget));
    }
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(x) => {
            note_stale(&reply.report);
            let next = hints("explain", &x);
            let mut partial = reply.partial;
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
            print_lines(cli, lines, &mut partial);
            print_partial(&partial);
            print_next(&next);
        }
        other => other_outcomes(cli, &reply.report, other, None),
    }
    code
}

/// Render `replace`: the range replaced and the range now held.
pub fn replace(cli: &Cli, reply: Reply<ReplaceResult>) -> ExitCode {
    if cli.json {
        return emit(&json::replace(reply, cli.budget));
    }
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(r) => {
            note_stale(&reply.report);
            println!(
                "replaced {}:{}-{} with {}:{}-{}",
                r.file, r.replaced.0, r.replaced.1, r.file, r.lines.0, r.lines.1
            );
        }
        other => other_outcomes(cli, &reply.report, other, None),
    }
    code
}

/// Ambiguous candidates print as a list (the trace rides in the envelope, or goes to stderr in
/// text); no match and stale file have no envelope, so the trace goes to stderr in both modes.
fn other_outcomes<T>(cli: &Cli, report: &Report, outcome: Outcome<T>, trace: Option<Trace>) {
    match outcome {
        Outcome::Ambiguous(candidates) => {
            eprintln!("katalogr: ambiguous: {} candidates", candidates.len());
            print_list(cli, report, candidates, Vec::new(), symbol_line, "", trace);
        }
        Outcome::NoMatch(msg) | Outcome::StaleFile(msg) => {
            eprintln!("katalogr: {msg}");
            print_trace(trace.as_ref());
        }
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

/// `82.1%`, or `-` for a rate with nothing to divide.
fn pct(rate: Option<f64>) -> String {
    rate.map_or("-".to_string(), |r| format!("{:.1}%", r * 100.0))
}

fn coverage_numbers(c: &CoverageCounts) -> String {
    format!(
        "files {}  resolved {} inferred {} unresolved {}  parse errors {}",
        c.files_indexed,
        pct(c.resolution_rate),
        pct(c.inferred_rate),
        pct(c.unresolved_rate),
        pct(c.parse_error_rate)
    )
}

fn coverage_group_rows(label: &str, rows: &[CoverageRow], out: &mut Vec<String>) {
    for r in rows {
        let key = r
            .repo
            .as_deref()
            .or(r.extension.as_deref())
            .or(r.language.as_deref())
            .unwrap_or("-");
        out.push(format!(
            "{label:<13} {key}  {}",
            coverage_numbers(&r.counts)
        ));
    }
}

pub const COVERAGE: Shape<CoverageReport> = Shape {
    header: |r| {
        let t = &r.totals;
        let mut v = vec![
            format!(
                "coverage  files {} indexed, {} parse errors ({}), {} unsupported, {} failed, {} unclaimed",
                t.files_indexed,
                t.files_parse_errors,
                pct(t.parse_error_rate),
                t.files_unsupported,
                t.files_failed,
                t.files_unclaimed
            ),
            format!(
                "refs      {}: {} resolved, {} inferred, {} unresolved; {} dangling contracts",
                t.refs_total,
                pct(t.resolution_rate),
                pct(t.inferred_rate),
                pct(t.unresolved_rate),
                t.contracts_dangling
            ),
        ];
        coverage_group_rows("by repo", &r.by_repo, &mut v);
        coverage_group_rows("by extension", &r.by_extension, &mut v);
        coverage_group_rows("by language", &r.by_language, &mut v);
        v
    },
    line: |g: &CoverageRow| {
        format!(
            "group         {}/{}/{}  {}",
            g.repo.as_deref().unwrap_or("-"),
            g.extension.as_deref().unwrap_or("-"),
            g.language.as_deref().unwrap_or("-"),
            coverage_numbers(&g.counts)
        )
    },
    footer: |r| {
        let mut v = Vec::new();
        for u in &r.unclaimed {
            let sfx: Vec<String> = u
                .suffixes
                .iter()
                .map(|s| format!("{} {}", s.suffix, s.files))
                .collect();
            v.push(format!(
                "unclaimed     {}  {} files: {}",
                u.repo,
                u.files,
                sfx.join(", ")
            ));
        }
        for m in &r.roots_missing {
            v.push(format!("roots missing {m}"));
        }
        v
    },
    fails: never,
};

/// Render an insight report as text or a JSON envelope. The budget trims whole `items`; the
/// summary stays and the envelope records the truncation.
pub fn insight<R>(cli: &Cli, reply: Reply<R>, command: &str, shape: &Shape<R>) -> ExitCode
where
    R: Itemized + Serialize + HintSource,
{
    if cli.json {
        return emit(&json::insight(reply, cli.budget, command, shape.fails));
    }
    let code = exit_for(&reply);
    match reply.outcome {
        Outcome::Found(mut r) => {
            note_stale(&reply.report);
            let failing = (shape.fails)(&r);
            let next = hints(command, &r);
            let mut partial = reply.partial;
            let lines: Vec<String> = r.items_mut().iter().map(|i| (shape.line)(i)).collect();
            for l in (shape.header)(&r) {
                println!("{l}");
            }
            print_lines(cli, lines, &mut partial);
            for l in (shape.footer)(&r) {
                println!("{l}");
            }
            print_partial(&partial);
            print_next(&next);
            if code == ExitCode::Ok && failing {
                return ExitCode::Error;
            }
        }
        other => other_outcomes(cli, &reply.report, other, None),
    }
    code
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
