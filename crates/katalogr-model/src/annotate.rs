//! Item annotations: finds board item, requirement and task references in free text
//! (FEAT-019 section 3, ADR-0016). The namespaces are neutral, so another tracker that uses
//! `CODE-n` ids fits the same helper.

use crate::fragment::ExportKey;
use regex::Regex;
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
}
