//! Shared helpers: file stems, path normalisation for `imports` and external `$ref`, and
//! the first paragraph of a documentation text.

use katalogr_model::{ExportKey, RelPath};

/// Namespace of the `file` export every artifact has (the engine's implicit file export).
pub const FILE_NS: &str = "file";

/// The file name without its last extension.
pub fn stem(path: &RelPath) -> &str {
    let file = path.as_str().rsplit('/').next().unwrap_or("");
    file.rsplit_once('.').map_or(file, |(s, _)| s)
}

/// True when `s` begins with a URI scheme (`http:`, `urn:` ...).
pub fn has_scheme(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once(':') else {
        return false;
    };
    let mut c = scheme.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic())
        && c.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

/// `location` joined to the directory of `from`, with `.` and `..` resolved and `/` as the
/// separator. None when the result leaves the repository root, is absolute, or has a URI
/// scheme.
pub fn normalise(from: &RelPath, location: &str) -> Option<String> {
    if location.is_empty() || has_scheme(location) || location.starts_with('/') {
        return None;
    }
    let location = location.replace('\\', "/");
    let mut parts: Vec<&str> = from.as_str().split('/').collect();
    parts.pop(); // the file name
    for seg in location.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    let joined = parts.join("/");
    (!joined.is_empty()).then_some(joined)
}

/// The key under which a file is exported: `<repo>:<path>`.
pub fn file_key(repo: &str, path: &str) -> ExportKey {
    ExportKey {
        namespace: FILE_NS.to_string(),
        key: format!("{repo}:{path}"),
    }
}

/// The first paragraph of a documentation text, whitespace collapsed; None when empty.
pub fn first_paragraph(text: &str) -> Option<String> {
    let para = text
        .trim()
        .split("\n\n")
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!para.is_empty()).then_some(para)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_paths() {
        let from = RelPath::new("schemas/a/order.xsd").unwrap();
        assert_eq!(
            normalise(&from, "common.xsd").as_deref(),
            Some("schemas/a/common.xsd")
        );
        assert_eq!(
            normalise(&from, "../common.xsd").as_deref(),
            Some("schemas/common.xsd")
        );
        assert_eq!(
            normalise(&from, "./x/../y.xsd").as_deref(),
            Some("schemas/a/y.xsd")
        );
        assert_eq!(
            normalise(&from, "../../../out.xsd"),
            None,
            "leaves the root"
        );
        assert_eq!(normalise(&from, "http://h/x.xsd"), None);
        assert_eq!(normalise(&from, "urn:x:y"), None);
        assert_eq!(normalise(&from, "/abs.xsd"), None);
        assert_eq!(normalise(&from, ""), None);
        let top = RelPath::new("o.xsd").unwrap();
        assert_eq!(normalise(&top, "../x.xsd"), None);
        assert!(has_scheme("https://a") && has_scheme("urn:a") && !has_scheme("a/b:c"));
        assert_eq!(
            first_paragraph("  One\n two.\n\nThree").as_deref(),
            Some("One two.")
        );
        assert_eq!(first_paragraph("  \n"), None);
    }
}
