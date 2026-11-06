//! The web view's pages (FEAT-033): `GET /` and `GET /assets/<name>`, answered from the bundle
//! embedded in the binary. No board, no index and no file is read.

use std::sync::OnceLock;

use super::Out;
use super::http::{HttpError, header};

/// The Content-Security-Policy of the page. `style-src` allows inline styles because Pele's SVG
/// carries its own; `script-src` has no `unsafe-inline` or `unsafe-eval`.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'none'; \
frame-ancestors 'none'";

/// The page and its entity tag, built once.
fn page() -> &'static (&'static str, String) {
    static PAGE: OnceLock<(&'static str, String)> = OnceLock::new();
    PAGE.get_or_init(|| {
        // One copy for the life of the process.
        let text: &'static str = Box::leak(katalogr_dist::web::page_linked("").into_boxed_str());
        let etag = katalogr_dist::web::etag(&katalogr_dist::web::Asset {
            name: "index.html",
            mime: "text/html",
            text,
        });
        (text, etag)
    })
}

/// Answers `/` and `/assets/<name>`. The caller has already run the limits, method, authorizer
/// and Host checks. An unknown path is a 404 `HttpError`.
pub fn answer(path: &str, headers: &[(String, String)]) -> Result<Out, HttpError> {
    let (body, etag, mime, page_headers) = if path == "/" {
        let (text, etag) = page();
        (
            *text,
            etag.clone(),
            "text/html; charset=utf-8".to_string(),
            true,
        )
    } else if let Some(a) = path
        .strip_prefix("/assets/")
        .and_then(katalogr_dist::web::asset)
    {
        (
            a.text,
            katalogr_dist::web::etag(a),
            a.mime.to_string(),
            false,
        )
    } else {
        return Err(HttpError::new(404, "not-found", "no such page"));
    };
    let mut out_headers: Vec<(&'static str, String)> = vec![
        ("Content-Type", mime),
        ("ETag", etag.clone()),
        ("Cache-Control", "no-cache".to_string()),
        ("X-Content-Type-Options", "nosniff".to_string()),
    ];
    if page_headers {
        out_headers.push(("Content-Security-Policy", CSP.to_string()));
        out_headers.push(("X-Frame-Options", "DENY".to_string()));
        out_headers.push(("Referrer-Policy", "no-referrer".to_string()));
    }
    let matches = header(headers, "if-none-match").is_some_and(|inm| {
        inm.split(',')
            .any(|t| t.trim().trim_start_matches("W/") == etag)
    });
    Ok(Out {
        status: if matches { 304 } else { 200 },
        body: if matches {
            String::new()
        } else {
            body.to_string()
        },
        headers: out_headers,
    })
}
