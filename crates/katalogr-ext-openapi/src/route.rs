//! The `http_route` contract kind and its key normaliser (FEAT-014 sections 5 and 6.2).
//!
//! A key is `<METHOD> <path>`: the method upper case (`*` for any), the path the full request
//! path without scheme, host, query, fragment, empty segments or trailing slash, in which every
//! parameter segment is `{}` and every catch-all `{*}`. The OpenAPI extractor, the request-line
//! guest and the profile rules (`normalise: http_route`) all go through this module, so their
//! keys are byte-identical.

use katalogr_model::KeyNormaliser;

/// The contract kind of every route key.
pub const CONTRACT_KIND: &str = "http_route";

/// The methods a key may carry; `*` matches any method.
pub const METHODS: [&str; 10] = [
    "GET", "PUT", "POST", "DELETE", "OPTIONS", "HEAD", "PATCH", "TRACE", "CONNECT", "*",
];

/// `<METHOD> <path>`; `None` when the method is not one of [`METHODS`] (after mapping) or the
/// target has no absolute path.
pub fn normalise(method: &str, target: &str) -> Option<String> {
    let method = method_of(method)?;
    let path = path_of(target)?;
    Some(format!("{method} {path}"))
}

/// Splits `raw` at the first space into method and target (no space: empty method, which means
/// `GET`), then calls [`normalise`].
pub fn normalise_raw(raw: &str) -> Option<String> {
    match raw.split_once(' ') {
        Some((method, target)) => normalise(method, target),
        None => normalise("", raw),
    }
}

/// The method of a key: trimmed, one surrounding pair of quotes removed, empty is `GET`, upper
/// case, `ANY` and `ALL` are `*`.
fn method_of(method: &str) -> Option<&'static str> {
    let m = method.trim();
    let m = m
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .or_else(|| m.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
        .unwrap_or(m);
    let upper = if m.is_empty() {
        "GET".to_string()
    } else {
        m.to_ascii_uppercase()
    };
    let upper = match upper.as_str() {
        "ANY" | "ALL" => "*",
        other => other,
    };
    METHODS.iter().find(|k| **k == upper).copied()
}

/// Length of a leading `scheme://`, when `s` starts with one.
fn scheme_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if !b.first()?.is_ascii_alphabetic() {
        return None;
    }
    let mut i = 1;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'+' | b'.' | b'-')) {
        i += 1;
    }
    s[i..].starts_with("://").then_some(i + 3)
}

/// The text after the authority: `s` without its scheme and `//authority`.
fn drop_authority(s: &str) -> Option<&str> {
    let rest = match scheme_len(s) {
        Some(n) => &s[n..],
        None => s.strip_prefix("//")?,
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some(&rest[end..])
}

/// The normalised path of a URL, path or template; `None` when it has no absolute path.
pub fn path_of(target: &str) -> Option<String> {
    let mut t = target.trim();
    while let Some(rest) = t.strip_prefix("{}") {
        t = rest;
    }
    let mut had_authority = false;
    if let Some(rest) = drop_authority(t) {
        t = rest;
        had_authority = true;
    }
    let t = t.split(['?', '#']).next().unwrap_or("");
    if t.is_empty() && had_authority {
        return Some("/".to_string());
    }
    if !t.starts_with('/') {
        return None;
    }
    let mut out = String::with_capacity(t.len());
    for seg in t.split('/').filter(|s| !s.is_empty()) {
        out.push('/');
        out.push_str(&norm_segment(seg));
    }
    if out.is_empty() {
        out.push('/');
    }
    Some(out)
}

/// One path segment, first matching rule wins.
fn norm_segment(s: &str) -> String {
    if s.starts_with('*') {
        return "{*}".to_string();
    }
    if s.starts_with(':') {
        return if s.ends_with('*') || s.ends_with('+') {
            "{*}".to_string()
        } else {
            "{}".to_string()
        };
    }
    if s.starts_with("{*") && s.ends_with('}') {
        return "{*}".to_string();
    }
    if s.len() >= 2 && s.starts_with('{') && s.ends_with('}') && !s[1..s.len() - 1].contains('{') {
        return "{}".to_string();
    }
    // Every innermost `{…}` of the segment becomes `{}`.
    let mut out = String::with_capacity(s.len());
    let mut open: Option<usize> = None;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => open = Some(i),
            '}' => {
                if let Some(o) = open.take() {
                    out.push_str(&s[start..o]);
                    out.push_str("{}");
                    start = i + 1;
                }
            }
            _ => {}
        }
    }
    out.push_str(&s[start..]);
    out
}

/// The path part of a server URL with no trailing `/` (`""` when none). Variables stay as
/// written; the caller substitutes their defaults first.
pub fn base_of(url: &str) -> String {
    let u = url.trim();
    let after = match u.find("://") {
        Some(i) => Some(&u[i + 3..]),
        None => u.strip_prefix("//"),
    };
    let rest = match after {
        Some(a) => &a[a.find(['/', '?', '#']).unwrap_or(a.len())..],
        None => u,
    };
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() || rest.starts_with('/') {
        rest.to_string()
    } else {
        format!("/{rest}")
    }
}

/// The `http_route` key normaliser: id `http_route`, version 1, [`normalise_raw`].
pub struct HttpRouteNormaliser;

/// The one instance the extension registers.
pub static HTTP_ROUTE: HttpRouteNormaliser = HttpRouteNormaliser;

impl KeyNormaliser for HttpRouteNormaliser {
    fn id(&self) -> &'static str {
        "http_route"
    }

    fn version(&self) -> u32 {
        1
    }

    fn normalise(&self, raw: &str) -> Option<String> {
        normalise_raw(raw)
    }
}
