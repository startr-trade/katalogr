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

fn needle(s: &str) -> Option<String> {
    let t = s.trim_end_matches(['/', '\\']);
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
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
    } else if ctx.needles.iter().any(|n| s.contains(n.as_str())) {
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
    path: &str,
    m: &std::collections::BTreeMap<String, Fact>,
) -> Result<(), GuardError> {
    for (k, v) in m {
        let p = format!("{path}.{k}");
        check(ctx, &p, k)?;
        fact(ctx, &p, v)?;
    }
    Ok(())
}

fn fact(ctx: &Ctx, path: &str, f: &Fact) -> Result<(), GuardError> {
    match f {
        Fact::Str(s) => check(ctx, path, s),
        Fact::Int(_) | Fact::Bool(_) => Ok(()),
        Fact::List(l) => l
            .iter()
            .enumerate()
            .try_for_each(|(i, v)| fact(ctx, &format!("{path}[{i}]"), v)),
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
    let mut needles: Vec<String> = home_dirs().iter().filter_map(|h| needle(h)).collect();
    needles.extend(kindex_root.and_then(|r| needle(r.as_str())));
    needles.extend(std::env::temp_dir().to_str().and_then(needle));
    let ctx = Ctx { needles };
    let c = &ctx;
    prov(c, "provenance", &f.provenance)?;
    check(c, "extension", &f.extension)?;
    check(c, "artifact.name", &f.artifact.name.canonical())?;
    facts(c, "artifact.facts", &f.artifact.facts)?;
    for (i, s) in f.symbols.iter().enumerate() {
        let p = format!("symbols[{i}]");
        check(c, &format!("{p}.name"), &s.name.canonical())?;
        check(c, &format!("{p}.kind"), &s.kind)?;
        facts(c, &format!("{p}.facts"), &s.facts)?;
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
