//! The katalogr favicon as a `data:` URI (FEAT-110). The brand SVG is embedded at compile time and
//! percent-encoded by a `const fn` at compile time, so the page and the export need no request
//! and no runtime encoding.

const SVG: &[u8] = include_str!("../../../assets/brand/katalogr-favicon.svg").as_bytes();
const PREFIX: &[u8] = b"data:image/svg+xml,";
const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// The bytes that must be percent-encoded: `%`, `#`, `<`, `>`, `"`, `:`, `/`, spaces, and line
/// breaks. `:` and `/` are encoded so the SVG namespace URL carries no literal `http://`.
const fn needs_escape(b: u8) -> bool {
    matches!(
        b,
        b'%' | b'#' | b'<' | b'>' | b'"' | b':' | b'/' | b' ' | b'\n' | b'\r' | b'\t'
    )
}

/// The length of the encoded URI: the prefix plus each byte, three bytes if escaped.
const fn uri_len() -> usize {
    let mut n = PREFIX.len();
    let mut i = 0;
    while i < SVG.len() {
        n += if needs_escape(SVG[i]) { 3 } else { 1 };
        i += 1;
    }
    n
}

/// The `data:image/svg+xml,` URI of the SVG, `N` bytes long (`N` is `uri_len()`).
const fn encode<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    let mut o = 0;
    while o < PREFIX.len() {
        out[o] = PREFIX[o];
        o += 1;
    }
    let mut i = 0;
    while i < SVG.len() {
        let b = SVG[i];
        if needs_escape(b) {
            out[o] = b'%';
            out[o + 1] = HEX[(b >> 4) as usize];
            out[o + 2] = HEX[(b & 0x0f) as usize];
            o += 3;
        } else {
            out[o] = b;
            o += 1;
        }
        i += 1;
    }
    out
}

const URI_LEN: usize = uri_len();
const URI: [u8; URI_LEN] = encode::<URI_LEN>();

/// The `href` of the page's icon link: the brand favicon as a `data:image/svg+xml` URI.
pub(crate) const FAVICON_HREF: &str = match std::str::from_utf8(&URI) {
    Ok(s) => s,
    Err(_) => panic!("the favicon URI is ASCII"),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn href_is_the_brand_svg() {
        let body = FAVICON_HREF
            .strip_prefix("data:image/svg+xml,")
            .expect("the svg media type");
        assert!(!body.contains(['<', '>', '"', ' ', '\n', '#', ':', '/']));
        let mut out = Vec::new();
        let raw = body.as_bytes();
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'%' {
                let hex = std::str::from_utf8(&raw[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 3;
            } else {
                out.push(raw[i]);
                i += 1;
            }
        }
        assert_eq!(out, SVG);
    }
}
