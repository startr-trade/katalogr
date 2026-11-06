//! What every lockfile parser shares: the entry model, the privacy cleaning of sources, and the
//! one function that turns entries into a sorted fragment (FEAT-007 R-6, R-7).

use katalogr_model::insight::{
    KIND_PACKAGE_EXTERNAL, KIND_PACKAGE_MEMBER, KIND_PACKAGE_VERSION, LINK_HAS_VERSION,
};
use katalogr_model::{
    ArtifactName, ArtifactNode, Confidence, ExtractError, ExtractInput, Fact, Fragment, Id, Link,
    LinkKind, Name, Provenance, Segment, Span, SymbolName, SymbolNode,
};
use katalogr_model::{Export, ExportKey};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// One lockfile entry, before sorting. `source` is raw; `entries_to_fragment` cleans it.
/// The field order is the sort order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LockEntry {
    pub name: String,
    pub version: String,
    pub source: Option<String>,
    /// Local to this project.
    pub member: bool,
    pub dev: bool,
    pub deps: Vec<DepRef>,
    /// The entry's lines, 1-based inclusive.
    pub lines: Option<(u32, u32)>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DepRef {
    pub name: String,
    pub version: Option<String>,
}

pub struct ParsedLock {
    /// Where package names are exported.
    pub namespace: &'static str,
    pub ecosystem: &'static str,
    /// The lockfile's version, as written.
    pub format: String,
    pub entries: Vec<LockEntry>,
    /// Recoverable: a dependency that names no entry.
    pub errors: Vec<ExtractError>,
}

const SECRET_WORDS: [&str; 6] = ["token", "auth", "key", "secret", "password", "sig"];

/// `None` for a local path. Otherwise the URL with no user, password or secret query parameter.
pub fn clean_source(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty()
        || raw.starts_with("file:")
        || raw.starts_with('/')
        || raw.starts_with("./")
        || raw.starts_with("../")
        || raw.starts_with("link:")
        || is_drive_path(raw)
    {
        return None;
    }
    // The fragment is kept as it is; the query loses its secrets.
    let (head, fragment) = match raw.split_once('#') {
        Some((h, f)) => (h, Some(f)),
        None => (raw, None),
    };
    let (base, query) = match head.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (head, None),
    };
    let mut out = strip_user(base);
    if let Some(q) = query {
        let kept: Vec<&str> = q
            .split('&')
            .filter(|p| {
                let name = p.split('=').next().unwrap_or("").to_ascii_lowercase();
                !SECRET_WORDS.iter().any(|w| name.contains(w))
            })
            .collect();
        if !kept.is_empty() {
            out.push('?');
            out.push_str(&kept.join("&"));
        }
    }
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    Some(out)
}

fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

/// Remove `user:password@` after `://`, or `user@` in the `user@host:path` form.
fn strip_user(base: &str) -> String {
    if let Some(i) = base.find("://") {
        let (scheme, rest) = base.split_at(i + 3);
        let end = rest.find('/').unwrap_or(rest.len());
        let authority = &rest[..end];
        // The last `@` ends the user part, so an `@` inside a password cannot leak.
        return match authority.rfind('@') {
            Some(at) => format!("{scheme}{}{}", &authority[at + 1..], &rest[end..]),
            None => base.to_string(),
        };
    }
    match (base.find('@'), base.find('/')) {
        (Some(at), slash) if slash.is_none_or(|s| at < s) => base[at + 1..].to_string(),
        _ => base.to_string(),
    }
}

/// The 1-based line of the first line at or after `from_line` (1-based) that equals `needle`
/// exactly, ignoring a trailing `\r`.
pub fn find_line(text: &str, needle: &str, from_line: u32) -> Option<u32> {
    let skip = from_line.saturating_sub(1) as usize;
    text.split('\n')
        .enumerate()
        .skip(skip)
        .find(|(_, l)| l.strip_suffix('\r').unwrap_or(l) == needle)
        .map(|(i, _)| i as u32 + 1)
}

fn bad_name(s: &str) -> bool {
    s.is_empty() || s.chars().any(char::is_control)
}

/// Sorted, cleaned nodes, links and exports. Pure. Never fails: an entry with a bad name is
/// dropped with an error in the returned list.
pub fn entries_to_fragment(
    input: &ExtractInput,
    lock: ParsedLock,
) -> (Fragment, Vec<ExtractError>) {
    let mut errors = lock.errors;
    let lens: Vec<u32> = input
        .content
        .split(|b| *b == b'\n')
        .map(|l| {
            let l = l.strip_suffix(b"\r").unwrap_or(l);
            String::from_utf8_lossy(l).chars().count().max(1) as u32
        })
        .collect();
    let total = lens.len().max(1) as u32;
    let line_len = |n: u32| lens.get(n as usize - 1).copied().unwrap_or(1);
    let span_of = |lines: Option<(u32, u32)>| -> Span {
        let (a, b) = match lines {
            Some((a, b)) if a >= 1 && b >= a && b <= total => (a, b),
            _ => (1, total),
        };
        Span {
            start_line: a,
            start_col: 1,
            end_line: b,
            end_col: line_len(b),
        }
    };

    // 1. Clean, sort, drop exact duplicates.
    let mut entries: Vec<LockEntry> = Vec::with_capacity(lock.entries.len());
    for mut e in lock.entries {
        if bad_name(&e.name) || (!e.member && bad_name(&e.version)) {
            errors.push(ExtractError::Parse {
                span: Some(span_of(e.lines)),
                message: format!("entry with an unusable name or version: {:?}", e.name),
            });
            continue;
        }
        e.source = e.source.as_deref().and_then(clean_source);
        entries.push(e);
    }
    entries.sort();
    entries.dedup();

    let artifact = ArtifactName {
        repo: input.repo.to_string(),
        path: input.path.clone(),
    };
    let provenance = Provenance {
        repo: input.repo.to_string(),
        path: input.path.clone(),
        revision: input.revision.to_string(),
    };
    let name_of = |segments: Vec<Segment>, ordinal: u32| SymbolName {
        artifact: artifact.clone(),
        segments,
        ordinal,
    };

    // 2. Nodes.
    struct Row {
        id: Id,
        span: Span,
    }
    let mut symbols: Vec<SymbolNode> = Vec::new();
    let mut links: Vec<Link> = Vec::new();
    let mut exports: Vec<Export> = Vec::new();
    let mut rows: Vec<Row> = Vec::with_capacity(entries.len());
    // Name nodes, in name order, with the first version's span.
    let mut names: BTreeMap<&str, (Id, Span)> = BTreeMap::new();
    // (name, version) -> ids by ordinal.
    let mut versions: HashMap<(&str, &str), Vec<Id>> = HashMap::new();
    let mut by_name: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut member_names: BTreeSet<&str> = BTreeSet::new();
    let mut member_ordinals: HashMap<&str, u32> = HashMap::new();
    let link = |from: Id, to: Id, kind: &str, span: Span| Link {
        from,
        to,
        kind: LinkKind(kind.to_string()),
        confidence: Confidence::Extracted,
        span: Some(span),
        provenance: provenance.clone(),
    };

    for e in &entries {
        let span = span_of(e.lines);
        if e.member {
            let n = member_ordinals.entry(&e.name).or_insert(0);
            *n += 1;
            let name = name_of(
                vec![Segment {
                    kind: "member".to_string(),
                    name: e.name.clone(),
                }],
                *n,
            );
            let id = name.id();
            let mut facts = BTreeMap::new();
            if !e.version.is_empty() {
                facts.insert("version".to_string(), Fact::Str(e.version.clone()));
            }
            symbols.push(SymbolNode {
                name,
                kind: KIND_PACKAGE_MEMBER.to_string(),
                span,
                facts,
            });
            member_names.insert(&e.name);
            rows.push(Row { id, span });
            continue;
        }
        let package = Segment {
            kind: "package".to_string(),
            name: e.name.clone(),
        };
        let name_id = match names.get(e.name.as_str()) {
            Some((id, _)) => *id,
            None => {
                let name = name_of(vec![package.clone()], 1);
                let id = name.id();
                let mut facts = BTreeMap::new();
                facts.insert(
                    "ecosystem".to_string(),
                    Fact::Str(lock.ecosystem.to_string()),
                );
                symbols.push(SymbolNode {
                    name,
                    kind: KIND_PACKAGE_EXTERNAL.to_string(),
                    span,
                    facts,
                });
                exports.push(Export {
                    key: ExportKey {
                        namespace: lock.namespace.to_string(),
                        key: e.name.clone(),
                    },
                    target: id,
                });
                names.insert(&e.name, (id, span));
                id
            }
        };
        let same = versions.entry((&e.name, &e.version)).or_default();
        let ordinal = same.len() as u32 + 1;
        let name = name_of(
            vec![
                package,
                Segment {
                    kind: "version".to_string(),
                    name: e.version.clone(),
                },
            ],
            ordinal,
        );
        let id = name.id();
        same.push(id);
        by_name.entry(&e.name).or_default().insert(&e.version);
        let mut facts = BTreeMap::new();
        facts.insert("version".to_string(), Fact::Str(e.version.clone()));
        if let Some(s) = &e.source {
            facts.insert("source".to_string(), Fact::Str(s.clone()));
        }
        if e.dev {
            facts.insert("dev".to_string(), Fact::Bool(true));
        }
        symbols.push(SymbolNode {
            name,
            kind: KIND_PACKAGE_VERSION.to_string(),
            span,
            facts,
        });
        links.push(link(name_id, id, LINK_HAS_VERSION, span));
        rows.push(Row { id, span });
    }

    // 3. Dependency links.
    for (e, row) in entries.iter().zip(&rows) {
        for dep in &e.deps {
            let target = match &dep.version {
                Some(v) => versions
                    .get(&(dep.name.as_str(), v.as_str()))
                    .map(|ids| ids[0]),
                None => match by_name.get(dep.name.as_str()) {
                    Some(vs) if vs.len() == 1 => {
                        let v = vs.iter().next().copied().unwrap_or("");
                        versions.get(&(dep.name.as_str(), v)).map(|ids| ids[0])
                    }
                    Some(_) => {
                        errors.push(ExtractError::Parse {
                            span: Some(row.span),
                            message: format!(
                                "dependency {:?} names no version and the file holds several",
                                dep.name
                            ),
                        });
                        continue;
                    }
                    None => None,
                },
            };
            match target {
                Some(to) if to != row.id => links.push(link(row.id, to, "depends-on", row.span)),
                Some(_) => {}
                // A local package depended on by name is not an external version.
                None if member_names.contains(dep.name.as_str()) => {}
                None => errors.push(ExtractError::Parse {
                    span: Some(row.span),
                    message: format!(
                        "dependency {:?}{} names no entry",
                        dep.name,
                        dep.version
                            .as_deref()
                            .map(|v| format!(" {v}"))
                            .unwrap_or_default()
                    ),
                }),
            }
        }
    }

    // 4. Sort as a fragment is sorted for storing.
    let mut keyed: Vec<(String, SymbolNode)> = symbols
        .into_iter()
        .map(|s| (s.name.canonical(), s))
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    let symbols = keyed.into_iter().map(|(_, s)| s).collect();
    links.sort_by(|a, b| (a.from, a.to, &a.kind.0).cmp(&(b.from, b.to, &b.kind.0)));
    links.dedup_by(|a, b| (a.from, a.to, &a.kind.0) == (b.from, b.to, &b.kind.0));
    exports.sort_by(|a, b| (&a.key.namespace, &a.key.key).cmp(&(&b.key.namespace, &b.key.key)));

    let mut facts = BTreeMap::new();
    facts.insert(
        "ecosystem".to_string(),
        Fact::Str(lock.ecosystem.to_string()),
    );
    facts.insert("lockfile_format".to_string(), Fact::Str(lock.format));
    (
        Fragment {
            provenance,
            extension: "deps".to_string(),
            artifact: ArtifactNode {
                name: artifact,
                facts,
            },
            symbols,
            links,
            refs: Vec::new(),
            exports,
            bindings: Vec::new(),
        },
        errors,
    )
}

#[cfg(test)]
#[test]
fn credentials_stripped() {
    assert_eq!(
        clean_source("https://bob:hunter2@registry.example/pkg.tgz").as_deref(),
        Some("https://registry.example/pkg.tgz")
    );
    assert_eq!(
        clean_source("git+ssh://git@host.example/org/repo.git#abc").as_deref(),
        Some("git+ssh://host.example/org/repo.git#abc")
    );
    assert_eq!(
        clean_source("deploy@host.example:org/repo.git").as_deref(),
        Some("host.example:org/repo.git")
    );
    // An `@` inside the password cannot leave the tail of it behind.
    assert_eq!(
        clean_source("https://u:p@ss@host.example/x").as_deref(),
        Some("https://host.example/x")
    );
    assert_eq!(
        clean_source("registry+https://registry.example/index").as_deref(),
        Some("registry+https://registry.example/index")
    );
}

#[cfg(test)]
#[test]
fn secret_query_params_removed() {
    assert_eq!(
        clean_source("https://h.example/p?rev=abc&Token=1&x_API_KEY=2&sig=3&a=b#frag").as_deref(),
        Some("https://h.example/p?rev=abc&a=b#frag")
    );
    assert_eq!(
        clean_source("https://h.example/p?auth=1&password=2&secret=3").as_deref(),
        Some("https://h.example/p")
    );
    assert_eq!(
        clean_source("https://h.example/p?rev=1#sha").as_deref(),
        Some("https://h.example/p?rev=1#sha")
    );
}

#[cfg(test)]
#[test]
fn local_path_source_not_stored() {
    for s in [
        "file:../vendor/a",
        "/abs/path",
        "./rel",
        "../rel",
        "link:../a",
        "C:/dir/a",
        "d:\\dir\\a",
        "",
    ] {
        assert_eq!(clean_source(s), None, "{s}");
    }
}

#[cfg(test)]
fn entry(
    name: &str,
    version: &str,
    source: Option<&str>,
    deps: &[(&str, Option<&str>)],
) -> LockEntry {
    LockEntry {
        name: name.to_string(),
        version: version.to_string(),
        source: source.map(str::to_string),
        member: false,
        dev: false,
        deps: deps
            .iter()
            .map(|(n, v)| DepRef {
                name: n.to_string(),
                version: v.map(str::to_string),
            })
            .collect(),
        lines: None,
    }
}

#[cfg(test)]
fn fragment_of(entries: Vec<LockEntry>) -> (Fragment, Vec<ExtractError>) {
    let path = katalogr_model::RelPath::new("Cargo.lock").unwrap();
    let text = "a\nb\nc\n";
    entries_to_fragment(
        &ExtractInput {
            repo: "app",
            path: &path,
            revision: "rev",
            content: text.as_bytes(),
            context: &[],
        },
        ParsedLock {
            namespace: "rs-package",
            ecosystem: "cargo",
            format: "3".to_string(),
            entries,
            errors: Vec::new(),
        },
    )
}

#[cfg(test)]
#[test]
fn output_is_sorted() {
    let mut member = entry("app", "0.1.0", None, &[("alpha-lib", Some("1.2.3"))]);
    member.member = true;
    let mut entries = vec![
        entry(
            "zeta",
            "2.0.0",
            Some("registry+https://r.example/"),
            &[("alpha-lib", None)],
        ),
        entry(
            "alpha-lib",
            "1.2.3",
            Some("registry+https://r.example/"),
            &[],
        ),
        entry(
            "alpha-lib",
            "1.0.0",
            Some("registry+https://r.example/"),
            &[],
        ),
        entry("alpha-lib", "1.2.3", Some("git+https://g.example/a"), &[]),
        member,
    ];
    let (a, _) = fragment_of(entries.clone());
    entries.reverse();
    let (b, _) = fragment_of(entries.clone());
    entries.rotate_left(2);
    let (c, _) = fragment_of(entries);
    assert_eq!(a, b);
    assert_eq!(a, c);
    let canon: Vec<String> = a.symbols.iter().map(|s| s.name.canonical()).collect();
    let mut sorted = canon.clone();
    sorted.sort();
    assert_eq!(canon, sorted);
    assert!(
        a.links
            .windows(2)
            .all(|w| (w[0].from, w[0].to) <= (w[1].from, w[1].to))
    );
    // Two sources for one version take ordinals in sorted source order.
    assert!(canon.contains(&"app:Cargo.lock#package:alpha-lib/version:1.2.3".to_string()));
    assert!(canon.contains(&"app:Cargo.lock#package:alpha-lib/version:1.2.3~2".to_string()));
    // One export per package name, none for members.
    let keys: Vec<&str> = a.exports.iter().map(|e| e.key.key.as_str()).collect();
    assert_eq!(keys, vec!["alpha-lib", "zeta"]);
}

#[cfg(test)]
#[test]
fn find_line_matches_exact_lines() {
    let text = "a\r\n  b\nb\nb\n";
    assert_eq!(find_line(text, "a", 1), Some(1));
    assert_eq!(find_line(text, "b", 1), Some(3));
    assert_eq!(find_line(text, "b", 4), Some(4));
    assert_eq!(find_line(text, "b", 5), None);
    assert_eq!(find_line(text, "c", 1), None);
}
