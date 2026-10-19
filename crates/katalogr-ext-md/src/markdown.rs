//! Markdown to symbols, links and references (FEAT-019 section 4, "Markdown").
//!
//! Headings become nested `section:<slug>` symbols; each link becomes a `links-to` reference
//! from the innermost section (or from the artifact before the first heading); text is scanned
//! for item annotations.

use crate::lines::Lines;
use katalogr_model::annotate;
use katalogr_model::{
    ArtifactName, Confidence, Export, ExportKey, Fact, Id, Link, LinkKind, Name, OutRef,
    Provenance, RelPath, Segment, Span, SymbolName, SymbolNode,
};
use pulldown_cmark::{Event, MetadataBlockKind, Options, Parser, Tag, TagEnd};
use std::collections::{BTreeMap, BTreeSet};

/// Namespace of the section anchors a document exports: `<path>#<anchor>`.
pub const NS_SECTION: &str = "md-section";
/// Namespace of every artifact's implicit file export (see the engine's stitcher).
pub const NS_FILE: &str = "file";
/// Namespace of board items, for monitor links.
pub use katalogr_model::annotate::ITEM_NS as NS_ITEM;

/// What one Markdown text contributes.
#[derive(Default)]
pub struct Parsed {
    pub symbols: Vec<SymbolNode>,
    pub links: Vec<Link>,
    pub refs: Vec<OutRef>,
    pub exports: Vec<Export>,
    /// Absolute URLs found in links, sorted and unique; they are facts only.
    pub external: Vec<String>,
    /// The first heading's text, if any.
    pub title: Option<String>,
    /// The text of a leading YAML metadata block, if any.
    pub front_matter: Option<String>,
}

struct Heading {
    level: u8,
    start: usize,
    text: String,
}

struct Open {
    level: u8,
    id: Id,
    segments: Vec<Segment>,
}

/// GitHub's heading anchor: lowercase, punctuation dropped except `-`, `_` and space, spaces
/// become `-`.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if c == ' ' {
            out.push('-');
        }
    }
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

/// Parse `text`, the content of `repo:path`.
pub fn parse(repo: &str, path: &RelPath, revision: &str, text: &str) -> Parsed {
    let artifact = ArtifactName {
        repo: repo.to_string(),
        path: path.clone(),
    };
    let aid = artifact.id();
    let provenance = Provenance {
        repo: repo.to_string(),
        path: path.clone(),
        revision: revision.to_string(),
    };
    let lines = Lines::new(text);
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let events: Vec<(Event<'_>, std::ops::Range<usize>)> =
        Parser::new_ext(text, opts).into_offset_iter().collect();

    // Pass 1: headings, to know where each section ends.
    let mut headings: Vec<Heading> = Vec::new();
    let mut in_heading = false;
    for (ev, range) in &events {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                in_heading = true;
                headings.push(Heading {
                    level: *level as u8,
                    start: range.start,
                    text: String::new(),
                });
            }
            Event::End(TagEnd::Heading(_)) => in_heading = false,
            Event::Text(t) | Event::Code(t) if in_heading => {
                if let Some(h) = headings.last_mut() {
                    h.text.push_str(t);
                }
            }
            _ => {}
        }
    }
    let section_span = |i: usize| -> Span {
        let start = headings[i].start;
        let end = headings[i + 1..]
            .iter()
            .find(|h| h.level <= headings[i].level)
            .map_or(text.len(), |h| h.start);
        let end = start + text[start..end].trim_end().len();
        lines.span(start as u32, end as u32)
    };

    // Pass 2: symbols, links and annotations in document order.
    let mut out = Parsed::default();
    let mut stack: Vec<Open> = Vec::new();
    let mut ordinals: BTreeMap<Vec<Segment>, u32> = BTreeMap::new();
    let mut anchors: BTreeMap<String, u32> = BTreeMap::new();
    let mut external: BTreeSet<String> = BTreeSet::new();
    let mut seen: BTreeSet<(Id, &'static str, Vec<ExportKey>)> = BTreeSet::new();
    let mut next_heading = 0usize;
    let mut front = String::new();
    let mut in_front = false;
    let dir = parent_dir(path.as_str());

    for (ev, range) in &events {
        let here = stack.last().map_or(aid, |o| o.id);
        let span = lines.span(range.start as u32, range.end as u32);
        match ev {
            Event::Start(Tag::MetadataBlock(MetadataBlockKind::YamlStyle)) => in_front = true,
            Event::End(TagEnd::MetadataBlock(_)) => in_front = false,
            Event::Text(t) if in_front => front.push_str(t),
            Event::Start(Tag::Heading { level, .. }) => {
                let i = next_heading;
                next_heading += 1;
                let level = *level as u8;
                while stack.last().is_some_and(|o| o.level >= level) {
                    stack.pop();
                }
                let slug = slug(&headings[i].text);
                let mut segments = stack.last().map_or_else(Vec::new, |o| o.segments.clone());
                segments.push(Segment {
                    kind: "section".to_string(),
                    name: slug.clone(),
                });
                let n = ordinals.entry(segments.clone()).or_insert(0);
                *n += 1;
                let name = SymbolName {
                    artifact: artifact.clone(),
                    segments: segments.clone(),
                    ordinal: *n,
                };
                let id = name.id();
                let sspan = section_span(i);
                if let Some(parent) = stack.last() {
                    out.links.push(Link {
                        from: parent.id,
                        to: id,
                        kind: LinkKind("contains".to_string()),
                        confidence: Confidence::Extracted,
                        span: Some(sspan),
                        provenance: provenance.clone(),
                    });
                }
                // GitHub numbers repeated anchors: `intro`, `intro-1`, `intro-2`.
                let count = anchors.entry(slug.clone()).or_insert(0);
                let anchor = if *count == 0 {
                    slug.clone()
                } else {
                    format!("{slug}-{count}")
                };
                *count += 1;
                out.exports.push(Export {
                    key: ExportKey {
                        namespace: NS_SECTION.to_string(),
                        key: format!("{}#{anchor}", path.as_str()),
                    },
                    target: id,
                });
                let mut facts = BTreeMap::new();
                facts.insert("level".to_string(), Fact::Int(level as i64));
                facts.insert("title".to_string(), Fact::Str(headings[i].text.clone()));
                facts.insert("anchor".to_string(), Fact::Str(anchor));
                if out.title.is_none() {
                    out.title = Some(headings[i].text.clone());
                }
                out.symbols.push(SymbolNode {
                    name,
                    kind: "section".to_string(),
                    span: sspan,
                    facts,
                });
                stack.push(Open {
                    level,
                    id,
                    segments,
                });
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                match link_target(repo, path, dir, dest_url) {
                    Target::External(url) => {
                        external.insert(url);
                    }
                    Target::Keys(keys) => {
                        if seen.insert((here, "links-to", keys.clone())) {
                            out.refs.push(OutRef {
                                from: here,
                                kind: LinkKind("links-to".to_string()),
                                span: Some(span),
                                candidates: keys,
                                confidence: Confidence::Extracted,
                                origin: None,
                                binds: None,
                            });
                        }
                    }
                    Target::Ignore => {}
                }
            }
            Event::Text(t) | Event::Code(t) => {
                for r in annotate::scan(t) {
                    let keys = annotate::keys(&r);
                    if seen.insert((here, "annotated-by", keys.clone())) {
                        out.refs.push(OutRef {
                            from: here,
                            kind: LinkKind("annotated-by".to_string()),
                            span: Some(span),
                            candidates: keys,
                            confidence: Confidence::Extracted,
                            origin: None,
                            binds: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    out.external = external.into_iter().collect();
    if !front.is_empty() {
        out.front_matter = Some(front);
    }
    out
}

/// What a link destination points at.
enum Target {
    /// An absolute URL (or other scheme): recorded as a fact.
    External(String),
    /// Candidate export keys, most specific first.
    Keys(Vec<ExportKey>),
    /// Nothing to resolve (empty, outside the repository, a directory, a monitor page other
    /// than an item).
    Ignore,
}

fn key(namespace: &str, key: String) -> ExportKey {
    ExportKey {
        namespace: namespace.to_string(),
        key,
    }
}

fn link_target(repo: &str, path: &RelPath, dir: &str, dest: &str) -> Target {
    let dest = dest.trim();
    if dest.is_empty() {
        return Target::Ignore;
    }
    if let Some(url) = email_url(dest) {
        return Target::External(url);
    }
    if has_scheme(dest) {
        return Target::External(dest.to_string());
    }
    if let Some(rest) = dest.strip_prefix("/p/") {
        // A kanbanr monitor link: /p/<project>/feature/<CODE>
        let parts: Vec<&str> = rest.split('/').collect();
        if let [_, "feature", code, ..] = parts.as_slice() {
            let code = code.split(['#', '?']).next().unwrap_or("");
            if is_code(code) {
                return Target::Keys(vec![key(NS_ITEM, code.to_string())]);
            }
        }
        return Target::Ignore;
    }
    if dest.starts_with('/') {
        return Target::Ignore;
    }
    let (before_frag, frag) = match dest.split_once('#') {
        Some((a, b)) => (a, Some(percent_decode(b))),
        None => (dest, None),
    };
    let file = before_frag.split('?').next().unwrap_or("");
    if file.is_empty() {
        return match frag {
            Some(f) if !f.is_empty() => {
                Target::Keys(vec![key(NS_SECTION, format!("{}#{f}", path.as_str()))])
            }
            _ => Target::Ignore,
        };
    }
    if file.ends_with('/') {
        return Target::Ignore;
    }
    let Some(resolved) = join_norm(dir, &percent_decode(file)) else {
        return Target::Ignore;
    };
    let mut keys = Vec::new();
    if let Some(f) = frag.filter(|f| !f.is_empty()) {
        keys.push(key(NS_SECTION, format!("{resolved}#{f}")));
    }
    keys.push(key(NS_FILE, format!("{repo}:{resolved}")));
    Target::Keys(keys)
}

/// The `mailto:` URL of an email destination, if `dest` is one: it starts with `mailto:`, or it
/// is a bare address (see [`is_addr_spec`]). pulldown-cmark's email autolinks take the second
/// form. Emails are URL facts, so the result is never a reference target.
fn email_url(dest: &str) -> Option<String> {
    if dest.starts_with("mailto:") {
        Some(dest.to_string())
    } else if is_addr_spec(dest) {
        Some(format!("mailto:{dest}"))
    } else {
        None
    }
}

/// Hand-written equivalent of `^[^@\s/]+@[^@\s/]+\.[^@\s/]+$`: exactly one `@`; no whitespace
/// or `/` anywhere; a non-empty local part; a domain with a `.` that has text on both sides.
fn is_addr_spec(s: &str) -> bool {
    let plain = |p: &str| !p.is_empty() && !p.chars().any(|c| c.is_whitespace() || c == '/');
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    if !plain(local) || !plain(domain) || domain.contains('@') {
        return false;
    }
    domain
        .match_indices('.')
        .any(|(i, _)| i >= 1 && i + 1 < domain.len())
}

fn is_code(s: &str) -> bool {
    let mut it = s.splitn(2, '-');
    match (it.next(), it.next()) {
        (Some(a), Some(b)) => {
            a.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && a.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && !b.is_empty()
                && b.chars().all(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

/// `scheme:` at the start (letters, digits, `+`, `-`, `.`, starting with a letter).
fn has_scheme(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && scheme.len() > 1
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `dir` joined with `rel`, with `.` and `..` resolved; `None` when it leaves the repository.
fn join_norm(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("Hello, World!"), "hello-world");
        assert_eq!(slug("A `code` Title"), "a-code-title");
        assert_eq!(slug("snake_case & more"), "snake_case--more");
        assert_eq!(slug("!!!"), "untitled");
    }

    #[test]
    fn joins() {
        assert_eq!(join_norm("a/b", "../c.md").as_deref(), Some("a/c.md"));
        assert_eq!(join_norm("", "x/./y").as_deref(), Some("x/y"));
        assert_eq!(join_norm("a", "../../x"), None);
    }

    #[test]
    fn schemes() {
        assert!(has_scheme("https://x.test/a"));
        assert!(has_scheme("mailto:a@b"));
        assert!(!has_scheme("a/b:c"));
        assert!(!has_scheme("C:"));
        assert!(!has_scheme("docs/a.md"));
    }

    #[test]
    fn emails() {
        assert!(is_addr_spec("someone@example.com"));
        assert!(is_addr_spec("a.b@c.d.e"));
        assert!(!is_addr_spec("someone@example"));
        assert!(!is_addr_spec("someone@.com"));
        assert!(!is_addr_spec("someone@example."));
        assert!(!is_addr_spec("@example.com"));
        assert!(!is_addr_spec("a@b@c.d"));
        assert!(!is_addr_spec("a b@c.d"));
        assert!(!is_addr_spec("docs/a@b.md"));
        assert_eq!(
            email_url("someone@example.com").as_deref(),
            Some("mailto:someone@example.com")
        );
        assert_eq!(
            email_url("mailto:someone@example.com").as_deref(),
            Some("mailto:someone@example.com")
        );
        assert_eq!(email_url("guide.md"), None);
    }
}
