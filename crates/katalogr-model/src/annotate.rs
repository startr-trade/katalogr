//! Item annotations: finds board item, requirement and task references in free text
//! (FEAT-019 section 3, ADR-0016). The namespaces are neutral, so another tracker that uses
//! `CODE-n` ids fits the same helper.

use crate::fragment::ExportKey;
use crate::graph::Fact;
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// Namespace under which a board item is exported (`FEAT-046`).
pub const ITEM_NS: &str = "board-item";
/// Namespace under which a requirement is exported (`FEAT-046/R-2`).
pub const REQ_NS: &str = "board-req";
/// Namespace under which an architecture decision record is exported (`ADR-0016`).
pub const ADR_NS: &str = "board-adr";
/// Namespace under which each item-code prefix the board uses is exported (`FEAT`).
pub const PREFIX_NS: &str = "board-prefix";
/// Namespace under which a charter goal is exported (`G-1`).
pub const GOAL_NS: &str = "board-goal";

/// A reference to an item, optionally narrowed to a requirement or a task.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ItemRef {
    /// `FEAT-046`.
    pub code: String,
    /// `R-2`.
    pub req: Option<String>,
    /// `TL-001/T3`.
    pub task: Option<String>,
}

/// `[kanbanr:]CODE` followed by an optional ` R-n`, `/R-n` or `/TL-n/Tn`.
static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:kanbanr:)?(?P<code>[A-Z][A-Z0-9]*-[0-9]+)(?:[ /]R-(?P<req>[0-9]+)|/(?P<task>TL-[0-9]+/T[0-9]+))?",
    )
    .expect("annotation pattern")
});

/// True when the byte before `at` continues an identifier.
fn glued_before(text: &str, at: usize) -> bool {
    text[..at]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

/// True when the byte at `at` continues an identifier.
fn glued_after(text: &str, at: usize) -> bool {
    text[at..]
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// Inside a URL: the whitespace-delimited word that holds `at` contains `://`.
fn in_url(text: &str, at: usize) -> bool {
    let start = text[..at].rfind(char::is_whitespace).map_or(0, |i| {
        i + text[i..].chars().next().map_or(1, char::len_utf8)
    });
    let end = text[at..]
        .find(char::is_whitespace)
        .map_or(text.len(), |i| at + i);
    text[start..end].contains("://")
}

/// Finds `<CODE>` / `<CODE> R-n` / `<CODE>/R-n` / `kanbanr:<CODE>[/R-n|/TL-n/Tn]` where
/// CODE = `[A-Z][A-Z0-9]*-\d+`; ignores matches inside URLs. In order of appearance, without
/// duplicates.
pub fn scan(text: &str) -> Vec<ItemRef> {
    let mut out: Vec<ItemRef> = Vec::new();
    for caps in PATTERN.captures_iter(text) {
        let whole = caps.get(0).expect("whole match");
        let code = caps.name("code").expect("code group");
        if glued_before(text, whole.start())
            || glued_after(text, whole.end())
            || in_url(text, whole.start())
        {
            continue;
        }
        let r = ItemRef {
            code: code.as_str().to_string(),
            req: caps.name("req").map(|m| format!("R-{}", m.as_str())),
            task: caps.name("task").map(|m| m.as_str().to_string()),
        };
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// True for a decision-record id (`ADR-0016`).
pub fn is_adr(code: &str) -> bool {
    code.starts_with("ADR-")
}

/// The prefix of an item code: `FEAT` for `FEAT-046`.
pub fn prefix_of(code: &str) -> &str {
    code.rsplit_once('-').map_or(code, |(p, _)| p)
}

/// The export keys that can resolve the reference, most specific first:
/// `[("board-req", "FEAT-046/R-2"), ("board-item", "FEAT-046")]`. A task reference resolves to
/// its item.
pub fn keys(r: &ItemRef) -> Vec<ExportKey> {
    if is_adr(&r.code) {
        return vec![ExportKey {
            namespace: ADR_NS.to_string(),
            key: r.code.clone(),
        }];
    }
    let mut out = Vec::new();
    if let Some(req) = &r.req {
        out.push(ExportKey {
            namespace: REQ_NS.to_string(),
            key: format!("{}/{req}", r.code),
        });
    }
    out.push(ExportKey {
        namespace: ITEM_NS.to_string(),
        key: r.code.clone(),
    });
    out
}

/// A `NOTE:`, `WHY:` or `RATIONALE:` marker found in one comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RationaleMarker {
    /// `NOTE`, `WHY` or `RATIONALE`.
    pub marker: String,
    /// 0-based line offset inside the comment text where the marker starts.
    pub line_offset: u32,
    /// Marker text, whitespace-collapsed, at most 240 chars (then `…`).
    pub text: String,
}

/// The marker words, upper case only (prose such as "Note that" is not a marker).
pub const RATIONALE_MARKERS: &[&str] = &["NOTE", "WHY", "RATIONALE"];

/// Longest marker text kept, in chars.
const MARKER_TEXT_MAX: usize = 240;

/// A comment line without its leading comment decoration (`/ * ! # -` and spaces) and any
/// closing `*/`.
fn strip_decoration(line: &str) -> &str {
    let l = line.trim_start_matches(|c: char| c.is_whitespace() || "/*!#-".contains(c));
    let l = l.trim_end();
    l.strip_suffix("*/").unwrap_or(l).trim_end()
}

/// `(marker, rest)` when `line` (already stripped) starts a marker:
/// `^(NOTE|WHY|RATIONALE)(\([^)]*\))?:\s*(.*)$`.
fn marker_start(line: &str) -> Option<(&'static str, &str)> {
    let word = RATIONALE_MARKERS.iter().find(|m| line.starts_with(**m))?;
    let mut rest = &line[word.len()..];
    if let Some(r) = rest.strip_prefix('(') {
        let close = r.find(')')?;
        rest = &r[close + 1..];
    }
    let rest = rest.strip_prefix(':')?;
    Some((word, rest.trim_start()))
}

fn finish_marker(marker: &str, line_offset: u32, text: &str, out: &mut Vec<RationaleMarker>) {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return;
    }
    let text = if collapsed.chars().count() > MARKER_TEXT_MAX {
        let mut t: String = collapsed.chars().take(MARKER_TEXT_MAX).collect();
        t.push('…');
        t
    } else {
        collapsed
    };
    out.push(RationaleMarker {
        marker: marker.to_string(),
        line_offset,
        text,
    });
}

/// Markers in one comment's raw text (delimiters included). Each line is stripped of leading
/// decoration; a marker's text continues over the following lines of the comment until a
/// blank line, a closing `*/` alone or another marker. An empty text drops the marker.
pub fn rationale_markers(comment: &str) -> Vec<RationaleMarker> {
    let mut out = Vec::new();
    // The marker being collected: (word, line offset, text so far).
    let mut open: Option<(&'static str, u32, String)> = None;
    for (idx, raw) in comment.lines().enumerate() {
        let line = strip_decoration(raw);
        if let Some((word, rest)) = marker_start(line) {
            if let Some((m, off, text)) = open.take() {
                finish_marker(m, off, &text, &mut out);
            }
            open = Some((word, idx as u32, rest.to_string()));
        } else if line.is_empty() {
            if let Some((m, off, text)) = open.take() {
                finish_marker(m, off, &text, &mut out);
            }
        } else if let Some((_, _, text)) = open.as_mut() {
            text.push(' ');
            text.push_str(line);
        }
    }
    if let Some((m, off, text)) = open.take() {
        finish_marker(m, off, &text, &mut out);
    }
    out
}

/// The `rationale` fact value for `markers`: a list of `{marker, line, text}` maps in the
/// order given. Each entry pairs the 1-based line of the comment's first line with its marker;
/// the recorded `line` is that line plus the marker's `line_offset`.
pub fn rationale_fact(markers: &[(u32, RationaleMarker)]) -> Fact {
    Fact::List(
        markers
            .iter()
            .map(|(line, m)| {
                Fact::Map(BTreeMap::from([
                    ("marker".to_string(), Fact::Str(m.marker.clone())),
                    (
                        "line".to_string(),
                        Fact::Int(i64::from(*line) + i64::from(m.line_offset)),
                    ),
                    ("text".to_string(), Fact::Str(m.text.clone())),
                ]))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> ItemRef {
        let mut v = scan(text);
        assert_eq!(v.len(), 1, "{text:?} -> {v:?}");
        v.remove(0)
    }

    fn item(code: &str, req: Option<&str>, task: Option<&str>) -> ItemRef {
        ItemRef {
            code: code.into(),
            req: req.map(Into::into),
            task: task.map(Into::into),
        }
    }

    #[test]
    fn forms() {
        assert_eq!(one("// FEAT-046"), item("FEAT-046", None, None));
        assert_eq!(
            one("// FEAT-046 R-2"),
            item("FEAT-046", Some("R-2"), None),
            "space form"
        );
        assert_eq!(
            one("see FEAT-046/R-2."),
            item("FEAT-046", Some("R-2"), None),
            "slash form"
        );
        assert_eq!(
            one("Refs: kanbanr:FEAT-046"),
            item("FEAT-046", None, None),
            "prefixed item"
        );
        assert_eq!(
            one("kanbanr:FEAT-046/R-7"),
            item("FEAT-046", Some("R-7"), None)
        );
        assert_eq!(
            one("kanbanr:FEAT-047/TL-002/T10"),
            item("FEAT-047", None, Some("TL-002/T10"))
        );
        // Several references, in order, without duplicates.
        assert_eq!(
            scan("Refs: kanbanr:A-1/R-2, kanbanr:B-3, kanbanr:A-1/R-2"),
            vec![item("A-1", Some("R-2"), None), item("B-3", None, None)]
        );
        // URLs, glued identifiers and lowercase codes are ignored.
        assert!(scan("https://example.test/FEAT-046/R-2 and http://x/y?FEAT-1").is_empty());
        assert!(scan("xFEAT-046 FEAT-046x feat-046 FEAT046 FEAT-").is_empty());
        // A monitor path is not a URL, so the code in it is found.
        assert_eq!(
            one("/p/demo/feature/FEAT-010"),
            item("FEAT-010", None, None)
        );
        // Keys, most specific first.
        let k = keys(&item("FEAT-046", Some("R-2"), None));
        assert_eq!(
            k.iter()
                .map(|e| (e.namespace.as_str(), e.key.as_str()))
                .collect::<Vec<_>>(),
            vec![("board-req", "FEAT-046/R-2"), ("board-item", "FEAT-046")]
        );
        let k = keys(&item("FEAT-046", None, Some("TL-001/T3")));
        assert_eq!(k.len(), 1);
        assert_eq!(k[0].namespace, "board-item");
    }

    #[test]
    fn rationale_marker_grammar() {
        let m = |c: &str| {
            rationale_markers(c)
                .into_iter()
                .map(|m| (m.marker, m.line_offset, m.text))
                .collect::<Vec<_>>()
        };
        let t = |a: &str, o: u32, b: &str| (a.to_string(), o, b.to_string());
        assert_eq!(m("// WHY: x"), vec![t("WHY", 0, "x")]);
        assert_eq!(m("/// NOTE(perf): a\n/// b"), vec![t("NOTE", 0, "a b")]);
        assert!(m("// Note that this is prose").is_empty());
        assert!(m("/* RATIONALE: */").is_empty());
        let long = "x".repeat(300);
        let got = m(&format!("// WHY: {long}"));
        assert_eq!(got[0].2, format!("{}…", "x".repeat(240)));
        assert_eq!(
            m("/*\n * NOTE: one\n * two\n * WHY: three\n *\n * stray\n */"),
            vec![t("NOTE", 1, "one two"), t("WHY", 3, "three")]
        );
        // Two markers in one comment are both found.
        assert_eq!(m("// NOTE: a\n// RATIONALE: b").len(), 2);
        // The fact carries absolute lines.
        let mk = rationale_markers("// x\n// WHY: y");
        let Fact::List(l) = rationale_fact(&[(10, mk[0].clone())]) else {
            panic!("list")
        };
        let Fact::Map(e) = &l[0] else { panic!("map") };
        assert_eq!(e["line"], Fact::Int(11));
    }
}
