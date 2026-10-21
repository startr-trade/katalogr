//! Value-level guard: no absolute paths, home directories or kindex roots in a fragment.

use camino::Utf8Path;
use katalogr_model::{Fact, Fragment, Name};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {reason}")]
pub struct GuardError {
    pub field: String,
    pub reason: String,
}

struct Ctx {
    needles: Vec<String>,
}

fn home_dirs() -> Vec<String> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .collect()
}

/// A guard needle built from `raw`, or None when it cannot be machine-specific: it must be an
/// absolute path (leading '/', a drive prefix `X:\` or `X:/`, or a leading `\\`) with at least
/// one character after the root once trailing separators are trimmed.
fn needle(raw: &str) -> Option<String> {
    let t = raw.trim_end_matches(['/', '\\']);
    let b = t.as_bytes();
    let root_len = if raw.starts_with('/') {
        1
    } else if raw.starts_with("\\\\") {
        2
    } else if raw.len() >= 3
        && raw.as_bytes()[0].is_ascii_alphabetic()
        && raw.as_bytes()[1] == b':'
        && matches!(raw.as_bytes()[2], b'\\' | b'/')
    {
        3
    } else {
        return None;
    };
    if b.len() > root_len {
        Some(t.to_string())
    } else {
        None
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// True when `needle` occurs in `hay` as a whole path: neither neighbour of the match is a word
/// character (alphanumeric, '_', '-' or '.').
fn contains_path(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(i, m)| {
        let before = hay[..i].chars().next_back();
        let after = hay[i + m.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// The needles for these raw values after the `needle` filter, in order, without duplicates.
#[doc(hidden)]
pub fn needles_from<'a>(raw: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in raw.into_iter().filter_map(needle) {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

fn check(ctx: &Ctx, field: &str, s: &str) -> Result<(), GuardError> {
    let b = s.as_bytes();
    // A leading '/' alone is not machine-specific: routes, topics and XPath start with one. Only
    // roots that exist per machine or per user are refused (FEAT-003 design amendment).
    const MACHINE_ROOTS: [&str; 8] = [
        "/home/",
        "/Users/",
        "/root/",
        "/tmp/",
        "/var/folders/",
        "/private/",
        "/mnt/",
        "/media/",
    ];
    let reason = if MACHINE_ROOTS.iter().any(|r| s.starts_with(r)) {
        Some("starts with a per-machine root")
    } else if s.starts_with("~/") {
        Some("starts with '~/'")
    } else if s.starts_with("\\\\") {
        Some("starts with a UNC prefix")
    } else if b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && matches!(b[2], b'\\' | b'/')
    {
        Some("starts with a drive prefix")
    } else if ctx.needles.iter().any(|n| contains_path(s, n)) {
        Some("contains the home directory, the temp directory or the kindex root")
    } else {
        None
    };
    match reason {
        Some(r) => Err(GuardError {
            field: field.to_string(),
            reason: format!("absolute path refused ({r})"),
        }),
        None => Ok(()),
    }
}

fn facts(
    ctx: &Ctx,
    path: &mut String,
    m: &std::collections::BTreeMap<String, Fact>,
) -> Result<(), GuardError> {
    let base = path.len();
    for (k, v) in m {
        path.push('.');
        path.push_str(k);
        let r = check(ctx, path, k).and_then(|()| fact(ctx, path, v));
        path.truncate(base);
        r?;
    }
    Ok(())
}

fn fact(ctx: &Ctx, path: &mut String, f: &Fact) -> Result<(), GuardError> {
    match f {
        Fact::Str(s) => check(ctx, path, s),
        Fact::Int(_) | Fact::Bool(_) => Ok(()),
        Fact::List(l) => {
            let base = path.len();
            for (i, v) in l.iter().enumerate() {
                path.push_str(&format!("[{i}]"));
                let r = fact(ctx, path, v);
                path.truncate(base);
                r?;
            }
            Ok(())
        }
        Fact::Map(m) => facts(ctx, path, m),
    }
}

fn prov(ctx: &Ctx, path: &str, p: &katalogr_model::Provenance) -> Result<(), GuardError> {
    check(ctx, &format!("{path}.repo"), &p.repo)?;
    check(ctx, &format!("{path}.path"), p.path.as_str())?;
    check(ctx, &format!("{path}.revision"), &p.revision)
}

/// Refuse any string in the fragment that starts with `/`, `~/`, a drive prefix or `\\`, or that
/// contains the user's home directory (HOME / USERPROFILE) or `kindex_root`. The error names the
/// field path (for example `symbols[3].facts.source`) and never echoes the value.
pub fn scan_for_absolute(f: &Fragment, kindex_root: Option<&Utf8Path>) -> Result<(), GuardError> {
    let homes = home_dirs();
    let temp = std::env::temp_dir();
    let raw = homes
        .iter()
        .map(String::as_str)
        .chain(kindex_root.map(|r| r.as_str()))
        .chain(temp.to_str());
    scan_for_absolute_with(f, &needles_from(raw))
}

/// As `scan_for_absolute` with explicit needles (already filtered by `needles_from`).
#[doc(hidden)]
pub fn scan_for_absolute_with(f: &Fragment, needles: &[String]) -> Result<(), GuardError> {
    let ctx = Ctx {
        needles: needles.to_vec(),
    };
    let c = &ctx;
    prov(c, "provenance", &f.provenance)?;
    check(c, "extension", &f.extension)?;
    check(c, "artifact.name", &f.artifact.name.canonical())?;
    facts(c, &mut "artifact.facts".to_string(), &f.artifact.facts)?;
    for (i, s) in f.symbols.iter().enumerate() {
        let p = format!("symbols[{i}]");
        check(c, &format!("{p}.name"), &s.name.canonical())?;
        check(c, &format!("{p}.kind"), &s.kind)?;
        facts(c, &mut format!("{p}.facts"), &s.facts)?;
    }
    for (i, l) in f.links.iter().enumerate() {
        let p = format!("links[{i}]");
        check(c, &format!("{p}.kind"), &l.kind.0)?;
        prov(c, &format!("{p}.provenance"), &l.provenance)?;
    }
    for (i, r) in f.refs.iter().enumerate() {
        let p = format!("refs[{i}]");
        check(c, &format!("{p}.kind"), &r.kind.0)?;
        for (j, k) in r.candidates.iter().enumerate() {
            check(c, &format!("{p}.candidates[{j}].namespace"), &k.namespace)?;
            check(c, &format!("{p}.candidates[{j}].key"), &k.key)?;
        }
    }
    for (i, e) in f.exports.iter().enumerate() {
        let p = format!("exports[{i}].key");
        check(c, &format!("{p}.namespace"), &e.key.namespace)?;
        check(c, &format!("{p}.key"), &e.key.key)?;
    }
    Ok(())
}
