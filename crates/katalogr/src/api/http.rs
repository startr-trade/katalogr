//! Request parsing and limits, the exit-to-status mapping, error bodies and `redact`
//! (FEAT-049 R-3, R-4, R-8, R-9). Pure functions over strings: no sockets and no katalogr calls.

use std::collections::BTreeMap;

use crate::exit::ExitCode;

/// Request line plus headers may not exceed this many bytes (431).
pub const MAX_HEAD_BYTES: usize = 8 * 1024;
/// `timeout` is capped at this many seconds.
pub const MAX_TIMEOUT_SECS: f64 = 120.0;
/// The versioned path prefix every op lives under.
pub const PREFIX: &str = "/api/v1/";
/// Parameters every op accepts, each with the CLI's meaning.
pub const COMMON_PARAMS: &[&str] = &[
    "context",
    "at",
    "consistency",
    "timeout",
    "budget",
    "diagnostic",
    "expand",
];

/// What authorisation sees of a request: method, path, query and headers. The fields are the
/// interface of a permission provider; the default `Localhost` reads none of them.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct RequestHead<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a str,
    pub headers: &'a [(String, String)],
}

impl RequestHead<'_> {
    /// The first header named `name`, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        header(self.headers, name)
    }
}

/// The first header named `name`, ignoring case.
pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// A refusal the server answers without calling katalogr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    pub status: u16,
    /// The `error` field of the body, a short kebab-case kind.
    pub kind: &'static str,
    pub message: String,
}

impl HttpError {
    pub fn new(status: u16, kind: &'static str, message: impl Into<String>) -> HttpError {
        HttpError {
            status,
            kind,
            message: message.into(),
        }
    }

    /// 400 with `error: usage`.
    pub fn usage(message: impl Into<String>) -> HttpError {
        HttpError::new(400, "usage", message)
    }

    /// The body: `{"schema":1,"error":<kind>,"message":<message>}`.
    pub fn body(&self) -> String {
        error_body(self.kind, &self.message)
    }

    /// The `Retry-After` header value, set on 503.
    pub fn retry_after(&self) -> Option<&'static str> {
        (self.status == 503).then_some("1")
    }
}

/// `{"schema":1,"error":<kind>,"message":<message>}`, compact.
pub fn error_body(kind: &str, message: &str) -> String {
    serde_json::json!({"schema": 1, "error": kind, "message": message}).to_string()
}

/// A parsed GET target: the op name and its query parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub op: String,
    pub params: BTreeMap<String, String>,
}

/// Parse a request: `head_len` is the byte length of the request line and headers.
///
/// 431 over [`MAX_HEAD_BYTES`]; 405 for any method but GET; 404 outside [`PREFIX`]; 400 for a
/// malformed or repeated parameter.
pub fn parse_target(method: &str, url: &str, head_len: usize) -> Result<Target, HttpError> {
    check_head(method, head_len)?;
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let op = path
        .strip_prefix(PREFIX)
        .filter(|op| !op.is_empty() && (!op.contains('/') || is_views_route(op)))
        .ok_or_else(|| HttpError::new(404, "not-found", "no such route"))?;
    let mut params = BTreeMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let (k, v) = (percent_decode(k)?, percent_decode(v)?);
        if params.insert(k.clone(), v).is_some() {
            return Err(HttpError::usage(format!("parameter `{k}` is repeated")));
        }
    }
    Ok(Target {
        op: op.to_string(),
        params,
    })
}

/// The limits every request meets first: 431 over [`MAX_HEAD_BYTES`], 405 for any method but GET.
pub fn check_head(method: &str, head_len: usize) -> Result<(), HttpError> {
    if head_len > MAX_HEAD_BYTES {
        return Err(HttpError::new(
            431,
            "header-too-large",
            format!("request head over {MAX_HEAD_BYTES} bytes"),
        ));
    }
    if method != "GET" {
        return Err(HttpError::new(
            405,
            "method-not-allowed",
            "only GET is served",
        ));
    }
    Ok(())
}

/// `views/` and `views/<op>`: the one place a route has a second segment (FEAT-088).
fn is_views_route(op: &str) -> bool {
    op.strip_prefix("views/").is_some_and(|r| !r.contains('/'))
}

/// Decode `%XX` and `+` (a space). A bad escape or non-UTF-8 is a 400.
fn percent_decode(s: &str) -> Result<String, HttpError> {
    let bad = || HttpError::usage("malformed percent-encoding in the query");
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = b.get(i + 1..i + 3).ok_or_else(bad)?;
                let hex = std::str::from_utf8(hex).map_err(|_| bad())?;
                out.push(u8::from_str_radix(hex, 16).map_err(|_| bad())?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| bad())
}

/// Refuse a parameter that is neither common nor in `op_params`, so a typo cannot silently
/// change an answer.
pub fn check_params(
    params: &BTreeMap<String, String>,
    op_params: &[&str],
) -> Result<(), HttpError> {
    match params
        .keys()
        .find(|k| !COMMON_PARAMS.contains(&k.as_str()) && !op_params.contains(&k.as_str()))
    {
        Some(k) => Err(HttpError::usage(format!("unknown parameter `{k}`"))),
        None => Ok(()),
    }
}

/// The `timeout` parameter in seconds, capped at [`MAX_TIMEOUT_SECS`]. A negative, non-finite or
/// non-numeric value is a 400.
pub fn capped_timeout(value: &str) -> Result<f64, HttpError> {
    let t: f64 = value
        .parse()
        .map_err(|_| HttpError::usage("`timeout` is not a number"))?;
    if !t.is_finite() || t < 0.0 {
        return Err(HttpError::usage("`timeout` must be a non-negative number"));
    }
    Ok(t.min(MAX_TIMEOUT_SECS))
}

/// The HTTP status for a CLI exit code (the table of FEAT-049's Design).
pub fn status_of(code: ExitCode) -> u16 {
    match code {
        ExitCode::Ok => 200,
        // A timed-out wait still answers, with `stale: true` in the body (ADR-0008).
        ExitCode::Stale => 200,
        ExitCode::Ambiguous | ExitCode::PositionUnavailable => 409,
        ExitCode::NoMatch => 404,
        ExitCode::Usage => 400,
        ExitCode::Error => 500,
    }
}

/// Remove absolute paths (POSIX `/…` and Windows `C:\…`) from a message, leaving `<path>`.
pub fn redact(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(i) = rest.find(is_path_start) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| c.is_whitespace() || "\"'`,;()<>[]{}".contains(c))
            .unwrap_or(tail.len());
        let token = &tail[..end];
        // Keep sentence punctuation that follows a path.
        let trimmed = token.trim_end_matches([':', '.', '!', '?']);
        if is_absolute(trimmed) {
            out.push_str("<path>");
            out.push_str(&token[trimmed.len()..]);
        } else if let Some((key, value)) = trimmed.split_once('=')
            && is_absolute(value)
        {
            // `key=/abs/path`
            out.push_str(key);
            out.push_str("=<path>");
            out.push_str(&token[trimmed.len()..]);
        } else {
            out.push_str(token);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn is_path_start(c: char) -> bool {
    c == '/' || c == '\\' || c.is_ascii_alphabetic()
}

/// An absolute path token: `/x…`, or `X:\…` or `X:/…`. A lone `/` is not a path.
fn is_absolute(t: &str) -> bool {
    let b = t.as_bytes();
    (b.len() > 1 && b[0] == b'/')
        || (b.len() > 2
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'\\' | b'/'))
        || (b.len() > 1 && b[0] == b'\\' && b[1] == b'\\')
}
