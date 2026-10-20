//! The request-line guest (FEAT-014 section 6.5; the guest side of ADR-0021): a string literal
//! that is exactly `<METHOD> <path or URL>`, optionally followed by an HTTP version, binds its
//! owner as a consumer of that route, inferred.

use crate::route;
use katalogr_model::{Binding, Confidence, ContractName, ContractRole, EmbeddedInput};

const REQUEST_METHODS: [&str; 9] = [
    "GET", "PUT", "POST", "DELETE", "OPTIONS", "HEAD", "PATCH", "TRACE", "CONNECT",
];

fn is_version(t: &str) -> bool {
    let Some(v) = t.strip_prefix("HTTP/") else {
        return false;
    };
    let b = v.as_bytes();
    match b {
        [d] => d.is_ascii_digit(),
        [d, b'.', e] => d.is_ascii_digit() && e.is_ascii_digit(),
        _ => false,
    }
}

/// The trimmed text is exactly `<METHOD> <target>` or `<METHOD> <target> HTTP/<d>[.<d>]`, single
/// spaces, METHOD upper case and one of the nine request methods, target starting with `/`,
/// `http://` or `https://` and containing no whitespace.
pub fn accepts(text: &str) -> bool {
    parts(text).is_some()
}

/// The method and target of an accepted text.
fn parts(text: &str) -> Option<(&str, &str)> {
    let t = text.trim();
    let mut it = t.split(' ');
    let method = it.next()?;
    let target = it.next()?;
    let version = it.next();
    if it.next().is_some() {
        return None;
    }
    if !REQUEST_METHODS.contains(&method) {
        return None;
    }
    if target.is_empty()
        || target.chars().any(char::is_whitespace)
        || !(target.starts_with('/')
            || target.starts_with("http://")
            || target.starts_with("https://"))
    {
        return None;
    }
    if let Some(v) = version
        && !is_version(v)
    {
        return None;
    }
    Some((method, target))
}

/// Holes filled with `{}`; one `Consumes` binding on `input.owner` when the key normalises.
pub fn extract(input: &EmbeddedInput) -> Vec<Binding> {
    let mut text = input.text.to_string();
    for &h in input.holes.iter().rev() {
        let h = h as usize;
        if h > text.len() || !text.is_char_boundary(h) {
            return Vec::new();
        }
        text.insert_str(h, "{}");
    }
    let Some((method, target)) = parts(&text) else {
        return Vec::new();
    };
    let Some(key) = route::normalise(method, target) else {
        return Vec::new();
    };
    vec![Binding {
        symbol: input.owner,
        contract: ContractName {
            kind: route::CONTRACT_KIND.to_string(),
            key,
        },
        role: ContractRole::Consumes,
        span: Some(input.span),
        confidence: Confidence::Inferred,
        origin: None,
    }]
}
