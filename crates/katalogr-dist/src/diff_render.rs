//! Markdown and one static HTML page for a [`DiffReport`] (FEAT-095 R-12 to R-14).
//!
//! Pure: nothing here reads a clock, a path or a setting, so the same report always gives the
//! same bytes. Both formats are made from the same sections, in the same order.

use crate::diff::DiffReport;
use katalogr_render::{ViewEdge, ViewGraph, ViewNode, head_tags, node_id, to_mermaid};
use std::collections::BTreeSet;

/// Edges the diagram holds at most; the rest are counted in the graph's `total`.
pub const DIAGRAM_EDGES: usize = 40;

/// One run of text in a cell.
enum Part {
    Text(String),
    /// A symbol's canonical name. Where `anchor` is set the symbol is defined; elsewhere it links
    /// to its definition when the report lists it.
    Sym {
        name: String,
        anchor: bool,
    },
}

type Cell = Vec<Part>;

fn t(s: impl Into<String>) -> Cell {
    vec![Part::Text(s.into())]
}

fn sym(name: &str, anchor: bool) -> Cell {
    vec![Part::Sym {
        name: name.to_string(),
        anchor,
    }]
}

enum Block {
    Heading(String),
    Para(String),
    Table {
        head: Vec<&'static str>,
        rows: Vec<Vec<Cell>>,
    },
}

struct Section {
    title: &'static str,
    blocks: Vec<Block>,
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

fn num(x: f64) -> String {
    format!("{x}")
}

/// "showing N of M" when a list was cut.
fn showing(shown: usize, total: u64) -> Option<Block> {
    (total > shown as u64).then(|| Block::Para(format!("Showing {shown} of {total}.")))
}

fn table(head: Vec<&'static str>, rows: Vec<Vec<Cell>>) -> Option<Block> {
    (!rows.is_empty()).then_some(Block::Table { head, rows })
}

fn push(blocks: &mut Vec<Block>, b: Option<Block>) {
    blocks.extend(b);
}

/// A list in report order: a heading, its table and its "showing" note, or nothing when empty.
fn listed(
    blocks: &mut Vec<Block>,
    heading: &str,
    head: Vec<&'static str>,
    rows: Vec<Vec<Cell>>,
    total: u64,
) {
    if rows.is_empty() {
        return;
    }
    let shown = rows.len();
    blocks.push(Block::Heading(heading.to_string()));
    push(blocks, table(head, rows));
    push(blocks, showing(shown, total));
}

fn sections(r: &DiffReport) -> Vec<Section> {
    let d = &r.diff;
    let s = &d.symbols;
    let mut out = Vec::new();

    // Summary.
    let mut blocks = vec![Block::Para(format!(
        "Repository `{}`, from `{}` to `{}`. Facts version `{}`. Each list is limited to {} entries.",
        r.repo,
        short(&r.a.commit),
        short(&r.b.commit),
        short(&r.facts_version),
        r.limit
    ))];
    let mut rows = vec![
        vec![t("Files compared"), t(s.files_changed.to_string())],
        vec![t("Symbols added"), t(s.added_total.to_string())],
        vec![t("Symbols removed"), t(s.removed_total.to_string())],
        vec![t("Symbols moved"), t(s.moved_total.to_string())],
        vec![
            t("Symbols with a changed signature"),
            t(s.changed_total.to_string()),
        ],
        vec![t("Links added"), t(d.links.links_added.len().to_string())],
        vec![
            t("Links removed"),
            t(d.links.links_removed.len().to_string()),
        ],
        vec![t("Contracts changed"), t(d.contracts_total.to_string())],
        vec![t("Hubs moved"), t(r.insights.hubs.len().to_string())],
        vec![
            t("Cycles"),
            t(format!(
                "{} to {}",
                r.insights.cycles.total_a, r.insights.cycles.total_b
            )),
        ],
        vec![
            t("Communities"),
            t(format!(
                "{} to {}",
                r.insights.communities.total_a, r.insights.communities.total_b
            )),
        ],
    ];
    for m in &d.links.metrics {
        rows.push(vec![
            t(m.name.clone()),
            t(format!("{} to {}", num(m.a), num(m.b))),
        ]);
    }
    push(&mut blocks, table(vec!["Measure", "Value"], rows));
    out.push(Section {
        title: "Summary",
        blocks,
    });

    // Symbols.
    let mut blocks = Vec::new();
    let refs = |v: &[katalogr_analytics::semantic_diff::SymbolRef]| -> Vec<Vec<Cell>> {
        v.iter()
            .map(|x| {
                vec![
                    sym(&x.name, true),
                    t(x.kind.clone()),
                    t(format!("{}:{}", x.path, x.line)),
                ]
            })
            .collect()
    };
    listed(
        &mut blocks,
        "Added",
        vec!["Symbol", "Kind", "Where"],
        refs(&s.added),
        s.added_total,
    );
    listed(
        &mut blocks,
        "Removed",
        vec!["Symbol", "Kind", "Where"],
        refs(&s.removed),
        s.removed_total,
    );
    listed(
        &mut blocks,
        "Moved",
        vec!["Symbol", "From", "To"],
        s.moved
            .iter()
            .map(|m| {
                vec![
                    sym(&m.to.name, true),
                    t(format!("{}:{}", m.from.path, m.from.line)),
                    t(format!("{}:{}", m.to.path, m.to.line)),
                ]
            })
            .collect(),
        s.moved_total,
    );
    listed(
        &mut blocks,
        "Changed",
        vec![
            "Symbol",
            "Kind before",
            "Signature before",
            "Signature after",
        ],
        s.changed
            .iter()
            .map(|c| {
                vec![
                    sym(&c.symbol.name, true),
                    t(c.kind_a.clone()),
                    t(c.signature_a.clone().unwrap_or_default()),
                    t(c.signature_b.clone().unwrap_or_default()),
                ]
            })
            .collect(),
        s.changed_total,
    );
    if blocks.is_empty() {
        blocks.push(Block::Para("No symbol changed.".into()));
    }
    out.push(Section {
        title: "Symbols",
        blocks,
    });

    // Links.
    let mut blocks = Vec::new();
    let link_rows = |v: &[katalogr_analytics::context_diff::LinkDelta]| -> Vec<Vec<Cell>> {
        v.iter()
            .map(|l| vec![sym(&l.from, false), t(l.kind.clone()), sym(&l.to, false)])
            .collect()
    };
    // A cut link list is marked in the limits section; its full size is not kept.
    let cap_total = |len: usize| len as u64;
    listed(
        &mut blocks,
        "Added",
        vec!["From", "Kind", "To"],
        link_rows(&d.links.links_added),
        cap_total(d.links.links_added.len()),
    );
    listed(
        &mut blocks,
        "Removed",
        vec!["From", "Kind", "To"],
        link_rows(&d.links.links_removed),
        cap_total(d.links.links_removed.len()),
    );
    let dangling = |v: &[katalogr_query::result::DanglingRef]| -> Vec<Vec<Cell>> {
        v.iter()
            .map(|x| {
                vec![
                    sym(&x.from.name, false),
                    t(x.kind.clone()),
                    t(x.wanted.join(", ")),
                ]
            })
            .collect()
    };
    listed(
        &mut blocks,
        "Newly dangling",
        vec!["From", "Kind", "Wanted"],
        dangling(&d.links.dangling_added),
        d.links.dangling_added.len() as u64,
    );
    listed(
        &mut blocks,
        "No longer dangling",
        vec!["From", "Kind", "Wanted"],
        dangling(&d.links.dangling_removed),
        d.links.dangling_removed.len() as u64,
    );
    if blocks.is_empty() {
        blocks.push(Block::Para("No link changed.".into()));
    }
    out.push(Section {
        title: "Links",
        blocks,
    });

    // Contracts.
    let mut blocks = Vec::new();
    let names = |v: &[String]| -> Cell {
        let mut c: Cell = Vec::new();
        for (i, n) in v.iter().enumerate() {
            if i > 0 {
                c.push(Part::Text(", ".into()));
            }
            c.push(Part::Sym {
                name: n.clone(),
                anchor: false,
            });
        }
        c
    };
    let rows: Vec<Vec<Cell>> = d
        .contracts
        .iter()
        .map(|c| {
            let ab = |a: u32, b: u32| t(format!("{a} to {b}"));
            let mut dang = String::new();
            if c.newly_dangling {
                dang.push_str("newly dangling");
            }
            if c.no_longer_dangling {
                dang.push_str("no longer dangling");
            }
            vec![
                t(c.contract.clone()),
                ab(c.a.providers, c.b.providers),
                ab(c.a.consumers, c.b.consumers),
                ab(c.a.declarers, c.b.declarers),
                names(&c.providers_added),
                names(&c.providers_removed),
                names(&c.consumers_added),
                names(&c.consumers_removed),
                names(&c.declarers_added),
                names(&c.declarers_removed),
                t(dang),
            ]
        })
        .collect();
    listed(
        &mut blocks,
        "Changed contracts",
        vec![
            "Contract",
            "Providers",
            "Consumers",
            "Declarers",
            "Providers added",
            "Providers removed",
            "Consumers added",
            "Consumers removed",
            "Declarers added",
            "Declarers removed",
            "Dangling",
        ],
        rows,
        d.contracts_total,
    );
    if blocks.is_empty() {
        blocks.push(Block::Para("No contract changed.".into()));
    }
    out.push(Section {
        title: "Contracts",
        blocks,
    });

    // Hubs.
    let mut blocks = Vec::new();
    let rows: Vec<Vec<Cell>> = r
        .insights
        .hubs
        .iter()
        .map(|h| {
            vec![
                sym(&h.name, false),
                t(h.kind.clone()),
                t(format!("{} to {}", num(h.degree_a), num(h.degree_b))),
                t(format!("{} to {}", num(h.rank_a), num(h.rank_b))),
            ]
        })
        .collect();
    push(
        &mut blocks,
        table(vec!["Hub", "Kind", "Degree", "Rank"], rows),
    );
    if blocks.is_empty() {
        blocks.push(Block::Para("No hub moved.".into()));
    }
    out.push(Section {
        title: "Hubs",
        blocks,
    });

    // Cycles.
    let c = &r.insights.cycles;
    let mut blocks = vec![Block::Para(format!(
        "Cycles: {} to {}. Nodes in a cycle: {} to {}.",
        c.total_a, c.total_b, c.cyclic_nodes_a, c.cyclic_nodes_b
    ))];
    let cyc = |v: &[Vec<String>]| -> Vec<Vec<Cell>> {
        v.iter()
            .map(|members| {
                let mut cell: Cell = Vec::new();
                for (i, n) in members.iter().enumerate() {
                    if i > 0 {
                        cell.push(Part::Text(", ".into()));
                    }
                    cell.push(Part::Sym {
                        name: n.clone(),
                        anchor: false,
                    });
                }
                vec![cell]
            })
            .collect()
    };
    listed(
        &mut blocks,
        "New cycles",
        vec!["Members"],
        cyc(&c.added),
        c.added.len() as u64,
    );
    listed(
        &mut blocks,
        "Gone cycles",
        vec!["Members"],
        cyc(&c.removed),
        c.removed.len() as u64,
    );
    out.push(Section {
        title: "Cycles",
        blocks,
    });

    // Communities.
    let m = &r.insights.communities;
    let mut blocks = vec![Block::Para(format!(
        "Communities: {} to {}. Modularity: {} to {}. Communities are matched by label, so a community whose label changed shows as one gone and one new.",
        m.total_a,
        m.total_b,
        num(m.modularity_a),
        num(m.modularity_b)
    ))];
    let summ = |v: &[katalogr_analytics::insight_shift::CommunitySummary]| -> Vec<Vec<Cell>> {
        v.iter()
            .map(|x| {
                vec![
                    t(x.label.clone()),
                    t(x.dir.clone().unwrap_or_default()),
                    t(x.size.to_string()),
                ]
            })
            .collect()
    };
    listed(
        &mut blocks,
        "New",
        vec!["Label", "Directory", "Size"],
        summ(&m.appeared),
        m.appeared.len() as u64,
    );
    listed(
        &mut blocks,
        "Gone",
        vec!["Label", "Directory", "Size"],
        summ(&m.disappeared),
        m.disappeared.len() as u64,
    );
    listed(
        &mut blocks,
        "Resized",
        vec!["Label", "Size before", "Size after"],
        m.resized
            .iter()
            .map(|x| {
                vec![
                    t(x.label.clone()),
                    t(x.size_a.to_string()),
                    t(x.size_b.to_string()),
                ]
            })
            .collect(),
        m.resized.len() as u64,
    );
    out.push(Section {
        title: "Communities",
        blocks,
    });

    // Coverage.
    let mut blocks = Vec::new();
    let rows: Vec<Vec<Cell>> = r
        .insights
        .coverage
        .iter()
        .map(|x| vec![t(x.name.clone()), t(num(x.a)), t(num(x.b)), t(num(x.delta))])
        .collect();
    push(
        &mut blocks,
        table(vec!["Measure", "Before", "After", "Change"], rows),
    );
    if blocks.is_empty() {
        blocks.push(Block::Para("No coverage measure moved.".into()));
    }
    out.push(Section {
        title: "Coverage",
        blocks,
    });

    // Limits of the comparison.
    let mut blocks = vec![Block::Para(
        "A rename inside a file is one symbol removed and one added. A change of line number is not a change. A file whose blob is the same on both sides compares as unchanged, even if its context changed. A symbol moves only when exactly one removed and one added symbol share a kind, name and ordinal.".into(),
    )];
    if !r.other_repos_differ.is_empty() {
        blocks.push(Block::Para(format!(
            "Other repositories differ between the sides ({}), so their rows may show as changes.",
            r.other_repos_differ.join(", ")
        )));
    }
    for p in &d.partial {
        blocks.push(Block::Para(p.line()));
    }
    out.push(Section {
        title: "Limits of the comparison",
        blocks,
    });
    out
}

// ---------------------------------------------------------------------------------- Markdown

fn md_cell(c: &str) -> String {
    c.replace('|', "\\|").replace(['\n', '\r'], "<br>")
}

fn md_part(p: &Part) -> String {
    match p {
        Part::Text(s) => s.clone(),
        Part::Sym { name, .. } => format!("`{}`", name.replace('`', "'")),
    }
}

/// The report as Markdown: tables, in the section order of the page.
pub fn to_markdown(r: &DiffReport) -> String {
    let mut out = format!(
        "# Changes in {} from {} to {}\n\n",
        r.repo,
        short(&r.a.commit),
        short(&r.b.commit)
    );
    for s in sections(r) {
        out.push_str(&format!("## {}\n\n", s.title));
        for b in &s.blocks {
            match b {
                Block::Heading(h) => out.push_str(&format!("### {h}\n\n")),
                Block::Para(p) => out.push_str(&format!("{p}\n\n")),
                Block::Table { head, rows } => {
                    out.push_str(&format!("| {} |\n", head.join(" | ")));
                    out.push_str(&format!("|{}|\n", vec![" --- "; head.len()].join("|")));
                    for row in rows {
                        let cells: Vec<String> = row
                            .iter()
                            .map(|c| md_cell(&c.iter().map(md_part).collect::<String>()))
                            .collect();
                        out.push_str(&format!("| {} |\n", cells.join(" | ")));
                    }
                    out.push('\n');
                }
            }
        }
    }
    out
}

// -------------------------------------------------------------------------------------- HTML

/// Escape text for HTML content and attribute values.
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// The anchor of a symbol.
fn anchor_id(name: &str) -> String {
    format!("s-{}", node_id(name))
}

/// Names with an anchor: every symbol in the symbols section.
fn anchored(r: &DiffReport) -> BTreeSet<String> {
    let s = &r.diff.symbols;
    s.added
        .iter()
        .map(|x| x.name.clone())
        .chain(s.removed.iter().map(|x| x.name.clone()))
        .chain(s.moved.iter().map(|x| x.to.name.clone()))
        .chain(s.changed.iter().map(|x| x.symbol.name.clone()))
        .collect()
}

/// The links of the changed relations as a graph, added first, then removed, in report order.
/// `None` when no link changed.
pub fn diff_graph(r: &DiffReport) -> Option<ViewGraph> {
    let links = &r.diff.links;
    let all: Vec<(&str, &katalogr_analytics::context_diff::LinkDelta)> = links
        .links_added
        .iter()
        .map(|l| ("added", l))
        .chain(links.links_removed.iter().map(|l| ("removed", l)))
        .collect();
    if all.is_empty() {
        return None;
    }
    let total = all.len() as u64;
    let mut nodes: std::collections::BTreeMap<String, ViewNode> = Default::default();
    let mut edges = Vec::new();
    for (what, l) in all.into_iter().take(DIAGRAM_EDGES) {
        for name in [&l.from, &l.to] {
            nodes.entry(node_id(name)).or_insert_with(|| ViewNode {
                id: node_id(name),
                name: name.clone(),
                label: String::new(),
                kind: String::new(),
                path: None,
                group: None,
            });
        }
        edges.push(ViewEdge {
            from: node_id(&l.from),
            to: node_id(&l.to),
            kind: format!("{what} {}", l.kind),
            inferred: false,
        });
    }
    edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));
    Some(ViewGraph {
        title: format!("diff {}..{}", short(&r.a.commit), short(&r.b.commit)),
        nodes: nodes.into_values().collect(),
        shown: edges.len() as u64,
        total,
        edges,
    })
}

const CSS: &str = "\
:root{color-scheme:light dark;--bg:#fff;--fg:#1b1f24;--muted:#57606a;--line:#d0d7de;--add:#1a7f37;--del:#cf222e;--link:#0969da}\n\
@media (prefers-color-scheme:dark){:root{--bg:#0d1117;--fg:#e6edf3;--muted:#8b949e;--line:#30363d;--add:#3fb950;--del:#f85149;--link:#58a6ff}}\n\
body{background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif;max-width:72rem;margin:0 auto;padding:1.5rem}\n\
h1{font-size:1.5rem}h2{font-size:1.2rem;cursor:pointer}h3{font-size:1rem;color:var(--muted)}\n\
table{border-collapse:collapse;margin:.5rem 0 1rem;width:100%}\n\
th,td{border:1px solid var(--line);padding:.25rem .5rem;text-align:left;vertical-align:top}\n\
code{font-family:ui-monospace,monospace;font-size:.9em}a{color:var(--link)}\n\
details{border-top:1px solid var(--line);margin-top:1rem}\n\
.diagram svg{max-width:100%;height:auto}\n";

fn html_part(p: &Part, names: &BTreeSet<String>, seen: &mut BTreeSet<String>) -> String {
    match p {
        Part::Text(s) => esc(s),
        Part::Sym { name, anchor } => {
            let code = format!("<code>{}</code>", esc(name));
            if *anchor && seen.insert(name.clone()) {
                format!("<span id=\"{}\">{code}</span>", anchor_id(name))
            } else if names.contains(name) {
                format!("<a href=\"#{}\">{code}</a>", anchor_id(name))
            } else {
                code
            }
        }
    }
}

/// The report as one self-contained page. The page makes no request: its content policy forbids
/// one, it holds no external reference, and with no changed link it holds no script at all.
pub fn to_html(r: &DiffReport) -> String {
    let names = anchored(r);
    let mut seen = BTreeSet::new();
    let graph = diff_graph(r);
    let title = format!(
        "Changes in {} from {} to {}",
        r.repo,
        short(&r.a.commit),
        short(&r.b.commit)
    );
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!(
        "<title>{}</title>\n<style>\n{CSS}</style>\n",
        esc(&title)
    ));
    if graph.is_some() {
        out.push_str(&head_tags(None));
    }
    out.push_str("</head>\n<body>\n");
    out.push_str(&format!("<h1>{}</h1>\n", esc(&title)));
    for s in sections(r) {
        out.push_str(&format!(
            "<details open>\n<summary><h2 style=\"display:inline\">{}</h2></summary>\n",
            esc(s.title)
        ));
        if s.title == "Links"
            && let Some(g) = &graph
        {
            out.push_str(&format!(
                "<pre><code class=\"language-mermaid\">{}</code></pre>\n",
                esc(&to_mermaid(g))
            ));
            if g.total > g.shown {
                out.push_str(&format!(
                    "<p>The diagram shows {} of {} changed links.</p>\n",
                    g.shown, g.total
                ));
            }
        }
        for b in &s.blocks {
            match b {
                Block::Heading(h) => out.push_str(&format!("<h3>{}</h3>\n", esc(h))),
                Block::Para(p) => out.push_str(&format!("<p>{}</p>\n", esc(p))),
                Block::Table { head, rows } => {
                    out.push_str("<table>\n<thead><tr>");
                    for h in head {
                        out.push_str(&format!("<th>{}</th>", esc(h)));
                    }
                    out.push_str("</tr></thead>\n<tbody>\n");
                    for row in rows {
                        out.push_str("<tr>");
                        for cell in row {
                            let text: String = cell
                                .iter()
                                .map(|p| html_part(p, &names, &mut seen))
                                .collect();
                            out.push_str(&format!("<td>{text}</td>"));
                        }
                        out.push_str("</tr>\n");
                    }
                    out.push_str("</tbody>\n</table>\n");
                }
            }
        }
        out.push_str("</details>\n");
    }
    out.push_str("</body>\n</html>\n");
    out
}
