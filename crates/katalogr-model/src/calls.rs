//! The neutral `str_calls` fact (FEAT-014, ADR-0020): the calls in a symbol that pass a
//! token-like string argument, rendered as normalised text with spans, plus the syntax-level
//! resolution of every argument that names a function or method. Language extractors fill it;
//! profile rules read it. Nothing here names a language, a framework or a protocol.
//!
//! Rendering grammar (shared by every extractor that records the fact):
//! - a call is `callee(arg, arg)`; a method call's callee is `<recv>.<name>` with the receiver
//!   rendered only when it is an identifier, a path, `self`/`this` or a member chain of those;
//! - a string argument is its decoded value, redacted, then quoted when token-like and `"…"`
//!   when it holds whitespace; template holes and non-literal concatenation operands are `{}`;
//! - an identifier, path or member chain is its dotted or `::` text; a closure or anything else
//!   is `_`; object literals sort their keys.

use crate::fragment::ExportKey;
use crate::graph::{Fact, Span};
use std::collections::BTreeMap;

/// Fact name: on a symbol (or on the artifact, for top-level code), the calls in its body that
/// pass at least one token-like string argument. Value: `Fact::List` of `Fact::Map`.
pub const STR_CALLS_FACT: &str = "str_calls";
/// Longest `text` kept, in bytes (the cut marker included).
pub const STR_CALL_TEXT_MAX: usize = 256;
/// Most entries kept per symbol; later calls are dropped.
pub const STR_CALLS_MAX: usize = 256;
/// Longest string value rendered verbatim.
pub const STR_VALUE_MAX: usize = 200;
/// Most argument references kept per entry; later ones are dropped.
pub const ARG_REFS_MAX: usize = 16;
/// Alphanumeric run length at which a value is taken for a secret.
pub const REDACT_RUN: usize = 24;

/// How an argument that names a function or method resolves, at syntax level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgRef {
    /// A symbol of the same fragment, by its name relative to the artifact (the canonical
    /// name text after the first `#`).
    Local(String),
    /// Export candidates of a name defined elsewhere, most specific first.
    Candidates(Vec<ExportKey>),
}

/// One recorded call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrCall {
    pub text: String,
    pub span: Span,
    /// Rendered argument text, its resolution, and whether the extractor would mark a reference
    /// through it inferred. In first-seen order; duplicates of a text keep the first.
    pub refs: Vec<(String, ArgRef, bool)>,
}

/// `"…"` with `\` and `"` backslash-escaped.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// True when `value` is non-empty, at most `STR_VALUE_MAX` bytes, and has no whitespace or
/// control characters.
pub fn token_like(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= STR_VALUE_MAX
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Drop URL userinfo (`scheme://user:pass@host` becomes `scheme://host`); then, if any run of
/// ASCII alphanumerics is `REDACT_RUN` or more long, return `<redacted>`. Else the value.
pub fn redact(value: &str) -> String {
    let mut s = value.to_string();
    if let Some(i) = s.find("://") {
        let start = i + 3;
        let rest = &s[start..];
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        if let Some(at) = rest[..end].rfind('@') {
            s.replace_range(start..start + at + 1, "");
        }
    }
    let mut run = 0usize;
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() {
            run += 1;
            if run >= REDACT_RUN {
                return "<redacted>".to_string();
            }
        } else {
            run = 0;
        }
    }
    s
}

/// Cut `text` to at most `STR_CALL_TEXT_MAX` bytes at a char boundary; when cut, the result ends
/// with `…` (U+2026) and still fits in the limit.
pub fn cap_text(text: String) -> String {
    if text.len() <= STR_CALL_TEXT_MAX {
        return text;
    }
    let mut end = STR_CALL_TEXT_MAX - '…'.len_utf8();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text;
    out.truncate(end);
    out.push('…');
    out
}

fn int(v: u32) -> Fact {
    Fact::Int(i64::from(v))
}

/// The fact value for `calls` (first `STR_CALLS_MAX` kept, each with at most `ARG_REFS_MAX`
/// refs), or `None` when `calls` is empty.
pub fn str_calls_fact(calls: &[StrCall]) -> Option<Fact> {
    if calls.is_empty() {
        return None;
    }
    let list = calls
        .iter()
        .take(STR_CALLS_MAX)
        .map(|c| {
            let mut m = BTreeMap::new();
            m.insert("text".to_string(), Fact::Str(c.text.clone()));
            m.insert("line".to_string(), int(c.span.start_line));
            m.insert("col".to_string(), int(c.span.start_col));
            m.insert("end_line".to_string(), int(c.span.end_line));
            m.insert("end_col".to_string(), int(c.span.end_col));
            let mut refs = BTreeMap::new();
            for (text, r, inferred) in &c.refs {
                if refs.len() >= ARG_REFS_MAX {
                    break;
                }
                if refs.contains_key(text) {
                    continue;
                }
                let mut e = BTreeMap::new();
                match r {
                    ArgRef::Local(rel) => {
                        e.insert("sym".to_string(), Fact::Str(rel.clone()));
                    }
                    ArgRef::Candidates(c) => {
                        e.insert(
                            "cands".to_string(),
                            Fact::List(
                                c.iter()
                                    .map(|k| Fact::Str(format!("{} {}", k.namespace, k.key)))
                                    .collect(),
                            ),
                        );
                    }
                }
                if *inferred {
                    e.insert("inferred".to_string(), Fact::Bool(true));
                }
                refs.insert(text.clone(), Fact::Map(e));
            }
            if !refs.is_empty() {
                m.insert("refs".to_string(), Fact::Map(refs));
            }
            Fact::Map(m)
        })
        .collect();
    Some(Fact::List(list))
}

/// The `refs` entry of one fact element, decoded: `None` when `text` has no entry or the entry is
/// malformed. Used by the profile engine.
pub fn arg_ref_of(element: &Fact, text: &str) -> Option<(ArgRef, bool)> {
    let Fact::Map(m) = element else { return None };
    let Some(Fact::Map(refs)) = m.get("refs") else {
        return None;
    };
    let Some(Fact::Map(e)) = refs.get(text) else {
        return None;
    };
    let inferred = matches!(e.get("inferred"), Some(Fact::Bool(true)));
    if let Some(Fact::Str(rel)) = e.get("sym") {
        return Some((ArgRef::Local(rel.clone()), inferred));
    }
    if let Some(Fact::List(items)) = e.get("cands") {
        let mut out = Vec::new();
        for it in items {
            let Fact::Str(s) = it else { return None };
            let (ns, key) = s.split_once(' ')?;
            out.push(ExportKey {
                namespace: ns.to_string(),
                key: key.to_string(),
            });
        }
        if out.is_empty() {
            return None;
        }
        return Some((ArgRef::Candidates(out), inferred));
    }
    None
}
