//! The vendored Pele build and the two small files that wire it into a page. Everything is
//! embedded in the binary; nothing is fetched at build or run time.

/// The pinned Pele version, the same one kanbanr uses.
pub const PELE_VERSION: &str = "1.7.0";

/// Pele's browser build; defines the global `pele`.
pub const PELE_JS: &[u8] = include_bytes!("../assets/pele/pele.global.js");
/// Turns each `pre > code.language-mermaid` into a `div.diagram`, or shows the source and the
/// error when Pele throws.
pub const PELE_INIT_JS: &[u8] = include_bytes!("../assets/pele/pele-init.js");
/// Maps Pele's variables onto the host theme.
pub const PELE_THEME_CSS: &[u8] = include_bytes!("../assets/pele/pele-theme.css");

/// File names the three assets are written under when they are linked rather than inlined.
pub const PELE_JS_NAME: &str = "pele.global.js";
pub const PELE_INIT_JS_NAME: &str = "pele-init.js";
pub const PELE_THEME_CSS_NAME: &str = "pele-theme.css";

fn text(bytes: &'static [u8]) -> &'static str {
    std::str::from_utf8(bytes).expect("embedded asset is UTF-8")
}

/// The tags a page needs: the stylesheet, Pele, then the init script.
///
/// With `Some(prefix)` the tags link `{prefix}pele-theme.css`, `{prefix}pele.global.js` and
/// `{prefix}pele-init.js`; the prefix is used as given, so it holds the relative path to the
/// directory the assets were written to and ends with `/` (one is added if missing). With
/// `None` the three files are inlined and the page needs nothing else.
pub fn head_tags(prefix: Option<&str>) -> String {
    match prefix {
        Some(p) => {
            let p = if p.is_empty() || p.ends_with('/') {
                p.to_string()
            } else {
                format!("{p}/")
            };
            format!(
                "<link rel=\"stylesheet\" href=\"{p}{PELE_THEME_CSS_NAME}\">\n\
                 <script src=\"{p}{PELE_JS_NAME}\"></script>\n\
                 <script src=\"{p}{PELE_INIT_JS_NAME}\"></script>\n"
            )
        }
        None => format!(
            "<style>\n{}</style>\n<script>\n{}</script>\n<script>\n{}</script>\n",
            text(PELE_THEME_CSS),
            inline_safe(text(PELE_JS)),
            inline_safe(text(PELE_INIT_JS)),
        ),
    }
}

/// A `</script` inside inline script would end the element early.
fn inline_safe(js: &str) -> String {
    let mut out = js.replace("</script", "<\\/script");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
const SHA256SUMS: &str = include_str!("../assets/pele/SHA256SUMS");
#[cfg(test)]
const LICENCE: &str = include_str!("../assets/pele/LICENSE");

#[cfg(test)]
fn pinned(file: &str) -> String {
    SHA256SUMS
        .lines()
        .filter_map(|l| l.split_once("  "))
        .find(|(_, name)| *name == file)
        .unwrap_or_else(|| panic!("{file} not in SHA256SUMS"))
        .0
        .to_string()
}

#[cfg(test)]
#[test]
fn vendored_matches_pin() {
    assert_eq!(sha256_hex(PELE_JS), pinned("pele.global.js"));
    assert_eq!(sha256_hex(LICENCE.as_bytes()), pinned("LICENSE"));
    let js = text(PELE_JS);
    assert!(
        js.starts_with(&format!("/*! Pele {PELE_VERSION} |")),
        "banner names the pinned version"
    );
}

#[cfg(test)]
#[test]
fn no_remote_reference() {
    // Our own files and the tags that link or inline them hold no remote URL at all.
    for (name, body) in [
        ("pele-init.js", text(PELE_INIT_JS)),
        ("pele-theme.css", text(PELE_THEME_CSS)),
    ] {
        assert!(
            !body.contains("http://") && !body.contains("https://") && !body.contains("//cdn"),
            "{name} names a remote URL"
        );
    }
    let linked = head_tags(Some("../katalogr-assets/"));
    assert!(
        !linked.contains("http:") && !linked.contains("https:") && !linked.contains("src=\"//")
    );
    // Pele holds its banner link in a comment and the SVG namespace; it loads nothing.
    let js = text(PELE_JS);
    for allowed in [
        "https://github.com/obsidianmd/pele",
        "http://www.w3.org/2000/svg",
    ] {
        assert!(js.contains(allowed));
    }
    let stripped = js
        .replace("https://github.com/obsidianmd/pele", "")
        .replace("http://www.w3.org/2000/svg", "");
    assert!(
        !stripped.contains("http://") && !stripped.contains("https://"),
        "Pele names another URL"
    );
    for call in ["fetch(", "XMLHttpRequest", "importScripts", "import("] {
        assert!(!js.contains(call), "Pele uses {call}");
    }
}

#[cfg(test)]
#[test]
fn licence_present() {
    assert!(LICENCE.contains("MIT License"));
    assert!(LICENCE.contains("Permission is hereby granted"));
}

#[cfg(test)]
const PELE_VARIABLES: [&str; 9] = [
    "--pele-bg",
    "--pele-fg",
    "--pele-muted",
    "--pele-line",
    "--pele-surface",
    "--pele-surface-alt",
    "--pele-border",
    "--pele-accent",
    "--pele-font",
];

#[cfg(test)]
#[test]
fn theme_maps_every_pele_variable() {
    let css = text(PELE_THEME_CSS);
    for v in PELE_VARIABLES {
        let at_root = css
            .split(":root")
            .nth(1)
            .and_then(|r| r.split('}').next())
            .unwrap_or("");
        assert!(
            at_root.contains(&format!("{v}:")),
            "{v} not mapped on :root"
        );
        // The light and dark blocks for katalogr's own pages set every colour too.
        if v != "--pele-font" {
            assert!(css.matches(&format!("{v}:")).count() >= 3, "{v} not themed");
        }
    }
}

#[cfg(test)]
#[test]
fn theme_uses_mdbook_variables() {
    let css = text(PELE_THEME_CSS);
    for (pele, mdbook) in [
        ("--pele-bg", "var(--bg)"),
        ("--pele-fg", "var(--fg)"),
        ("--pele-muted", "var(--icons)"),
        ("--pele-line", "var(--fg)"),
        ("--pele-surface", "var(--quote-bg)"),
        ("--pele-surface-alt", "var(--table-alternate-bg)"),
        ("--pele-border", "var(--table-border-color)"),
        ("--pele-accent", "var(--links)"),
        ("--pele-font", "var(--main-font, inherit)"),
    ] {
        assert!(
            css.contains(&format!("{pele}: {mdbook};")),
            "{pele} should be {mdbook}"
        );
    }
}

#[cfg(test)]
#[test]
fn init_shows_source_and_error() {
    let js = text(PELE_INIT_JS);
    assert!(js.contains("pre > code.language-mermaid"));
    assert!(js.contains("div") && js.contains("\"diagram\""));
    assert!(js.contains("katalogr-error"));
    // The source and the error text are set as text, never as markup; the one innerHTML is
    // Pele's own SVG.
    assert_eq!(js.matches("innerHTML").count(), 1);
    assert!(js.contains("body.textContent = source"));
    assert!(js.contains("head.textContent"));
    assert!(js.contains("catch (e)"));
}

#[cfg(test)]
#[test]
fn head_tags_inline_and_linked() {
    let inline = head_tags(None);
    assert!(inline.contains("<style>") && inline.contains("pele.render"));
    assert!(!inline.contains("</script></script>"));
    let linked = head_tags(Some("../.."));
    assert!(linked.contains("href=\"../../pele-theme.css\""));
    assert!(linked.contains("src=\"../../pele.global.js\""));
    assert!(linked.contains("src=\"../../pele-init.js\""));
}
