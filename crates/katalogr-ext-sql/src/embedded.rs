//! The guest side (§6.6): SQL inside string literals of host code, bound as inferred
//! consumers of table and routine contracts.

use crate::context::schema_from_embedded;
use crate::names::routine_key;
use crate::refs::{Collector, as_strs, name_parts, parse_chain, relations};
use crate::scan::{Refs, scan_body};
use crate::{CONTRACT_ROUTINE, CONTRACT_TABLE};
use katalogr_model::{Binding, Confidence, ContractName, ContractRole, EmbeddedInput, Name};
use sqlparser::ast::Statement;

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The first word after leading whitespace, comments and `(`, and the offset after it.
fn first_word(b: &[u8]) -> Option<(&[u8], usize)> {
    let mut i = 0;
    loop {
        match b.get(i)? {
            c if c.is_ascii_whitespace() || *c == b'(' => i += 1,
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            _ => break,
        }
    }
    let start = i;
    while i < b.len() && is_ident(b[i]) {
        i += 1;
    }
    (i > start).then(|| (&b[start..i], i))
}

/// True when `word` occurs in `b` at a word boundary.
fn has_word(b: &[u8], word: &[u8]) -> bool {
    if b.len() < word.len() {
        return false;
    }
    (0..=b.len() - word.len()).any(|i| {
        b[i..i + word.len()].eq_ignore_ascii_case(word)
            && (i == 0 || !is_ident(b[i - 1]))
            && b.get(i + word.len()).is_none_or(|c| !is_ident(*c))
    })
}

/// A cheap test of whether `text` is worth parsing: O(len), no allocation.
pub fn accepts(text: &str) -> bool {
    let b = text.as_bytes();
    let Some((word, end)) = first_word(b) else {
        return false;
    };
    let rest = &b[end..];
    let paired = if word.eq_ignore_ascii_case(b"select") || word.eq_ignore_ascii_case(b"delete") {
        has_word(rest, b"from")
    } else if word.eq_ignore_ascii_case(b"insert")
        || word.eq_ignore_ascii_case(b"replace")
        || word.eq_ignore_ascii_case(b"merge")
    {
        has_word(rest, b"into")
    } else if word.eq_ignore_ascii_case(b"update") {
        has_word(rest, b"set")
    } else if word.eq_ignore_ascii_case(b"with") {
        [&b"select"[..], b"insert", b"update", b"delete"]
            .iter()
            .any(|w| has_word(rest, w))
    } else if word.eq_ignore_ascii_case(b"call") {
        rest.contains(&b'(')
    } else {
        return false;
    };
    if !paired {
        return false;
    }
    // Prose guard: upper-case first word, or a SQL marker and no sentence ending.
    if word.iter().all(|c| !c.is_ascii_lowercase()) {
        return true;
    }
    let marker = b
        .iter()
        .any(|c| matches!(c, b'*' | b'=' | b'(' | b'?' | b'$'));
    let last = text.trim_end().as_bytes().last().copied();
    marker && !matches!(last, Some(b'.') | Some(b'!'))
}

/// Replace each hole with ` NULL `, in descending offset order.
fn fill_holes(text: &str, holes: &[u32]) -> String {
    let mut s = text.to_string();
    for &h in holes.iter().rev() {
        let mut at = (h as usize).min(s.len());
        while !s.is_char_boundary(at) {
            at -= 1;
        }
        s.insert_str(at, " NULL ");
    }
    s
}

/// Replace every `{[^{}]*}` with `NULL` (Rust format arguments).
fn replace_braces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '{'
            && let Some(close) = text[i + 1..].find(['{', '}'])
            && text.as_bytes()[i + 1 + close] == b'}'
        {
            out.push_str("NULL");
            let end = i + 1 + close;
            while chars.peek().is_some_and(|(j, _)| *j <= end) {
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn stmts_refs(stmts: &[Statement], schema: &str) -> Refs {
    let mut r = Refs::default();
    for st in stmts {
        let one = relations(st, schema);
        r.tables.extend(one.tables);
        r.routines.extend(one.routines);
        if let Statement::Call(f) = st {
            let parts = name_parts(&f.name);
            r.routines.insert(routine_key(&as_strs(&parts), schema));
            let c = Collector::of(f).refs(schema);
            r.tables.extend(c.tables);
            r.routines.extend(c.routines);
        }
    }
    r.tables.retain(|k| !k.ends_with(".null"));
    r
}

/// References of embedded text (§6.6): parse with the auto chain, then the brace retry, then
/// the lexical scan. Never fails.
pub fn embedded_refs(text: &str, holes: &[u32], default_schema: &str) -> Refs {
    let text = fill_holes(text, holes);
    if let Ok((stmts, _)) = parse_chain(&text, None) {
        return stmts_refs(&stmts, default_schema);
    }
    if text.contains('{') {
        let replaced = replace_braces(&text);
        if let Ok((stmts, _)) = parse_chain(&replaced, None) {
            return stmts_refs(&stmts, default_schema);
        }
        return scan_body(&replaced, default_schema);
    }
    scan_body(&text, default_schema)
}

/// Bindings for one embedded text: consumes of each table and routine it names.
pub fn extract(input: &EmbeddedInput) -> Vec<Binding> {
    let schema = schema_from_embedded(input.context);
    let refs = embedded_refs(input.text, input.holes, schema);
    let mk = |kind: &str, key: &String| Binding {
        symbol: input.owner,
        contract: ContractName {
            kind: kind.to_string(),
            key: key.clone(),
        },
        role: ContractRole::Consumes,
        span: Some(input.span),
        confidence: Confidence::Inferred,
        origin: None,
    };
    let mut out: Vec<Binding> = refs
        .routines
        .iter()
        .map(|k| mk(CONTRACT_ROUTINE, k))
        .chain(refs.tables.iter().map(|k| mk(CONTRACT_TABLE, k)))
        .collect();
    out.sort_by_key(|b| b.contract.canonical());
    out
}
