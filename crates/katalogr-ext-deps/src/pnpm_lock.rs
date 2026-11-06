//! `pnpm-lock.yaml`, versions 6 and 9 (FEAT-007 R-2, R-4, R-5).

use crate::lock_common::{DepRef, LockEntry, ParsedLock};
use katalogr_model::ExtractError;
use serde_yaml_ng::{Mapping, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

fn parse_err(message: String) -> ExtractError {
    ExtractError::Parse {
        span: None,
        message,
    }
}

/// `text` without a `(…)` suffix.
fn strip_suffix(text: &str) -> &str {
    text.find('(').map_or(text, |i| &text[..i])
}

/// `name@version` with an optional leading `/` and an optional `(…)` suffix: the name and the
/// version, split at the last `@` after the first character.
pub fn split_key(key: &str) -> Option<(String, String)> {
    let key = strip_suffix(key.trim());
    let key = key.strip_prefix('/').unwrap_or(key);
    let at = key.rfind('@').filter(|i| *i > 0)?;
    let (name, version) = (&key[..at], &key[at + 1..]);
    (!name.is_empty() && !version.is_empty()).then(|| (name.to_string(), version.to_string()))
}

fn is_local(version: &str) -> bool {
    ["link:", "workspace:", "file:"]
        .iter()
        .any(|p| version.starts_with(p))
}

/// A dependency as the lockfile writes it: a plain version, or a `real@version` alias.
fn dep_ref(name: &str, value: &str) -> Option<DepRef> {
    let value = strip_suffix(value.trim());
    if value.is_empty() || is_local(value) {
        return None;
    }
    if value.contains('@') {
        return split_key(value).map(|(name, version)| DepRef {
            name,
            version: Some(version),
        });
    }
    Some(DepRef {
        name: name.to_string(),
        version: Some(value.to_string()),
    })
}

fn deps_of(v: &Value, fields: &[&str]) -> Vec<DepRef> {
    let mut out = Vec::new();
    for f in fields {
        let Some(m) = v.get(f).and_then(Value::as_mapping) else {
            continue;
        };
        for (k, val) in m {
            let Some(name) = k.as_str() else { continue };
            // An importer's value is `{specifier, version}`; a snapshot's is the version.
            let version = match val {
                Value::String(s) => Some(s.as_str()),
                Value::Mapping(_) => val.get("version").and_then(Value::as_str),
                _ => None,
            };
            if let Some(r) = version.and_then(|v| dep_ref(name, v)) {
                out.push(r);
            }
        }
    }
    out
}

/// Entry keys of the `packages:` section with the lines each covers, one pass over the text.
/// A key is written `  key:` or `  'key':` at two spaces of indent.
fn package_lines(text: &str) -> HashMap<String, (u32, u32)> {
    let mut out: HashMap<String, (u32, u32)> = HashMap::new();
    let mut in_packages = false;
    let mut open: Option<(String, u32)> = None;
    let mut last_content = 0u32;
    let close =
        |open: &mut Option<(String, u32)>, end: u32, out: &mut HashMap<String, (u32, u32)>| {
            if let Some((k, s)) = open.take() {
                out.entry(k).or_insert((s, end.max(s)));
            }
        };
    for (i, line) in text.split('\n').enumerate() {
        let n = i as u32 + 1;
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(' ') {
            close(&mut open, last_content, &mut out);
            in_packages = line == "packages:";
        } else if in_packages
            && line.starts_with("  ")
            && !line.starts_with("   ")
            && let Some(k) = line[2..].trim_end().strip_suffix(':')
        {
            close(&mut open, last_content, &mut out);
            let k = k
                .strip_prefix('\'')
                .and_then(|k| k.strip_suffix('\''))
                .or_else(|| k.strip_prefix('"').and_then(|k| k.strip_suffix('"')))
                .unwrap_or(k);
            open = Some((k.to_string(), n));
        }
        last_content = n;
    }
    close(&mut open, last_content, &mut out);
    out
}

pub fn parse(text: &str) -> Result<ParsedLock, ExtractError> {
    let doc: Value = serde_yaml_ng::from_str(text).map_err(|e| parse_err(e.to_string()))?;
    let root = doc
        .as_mapping()
        .ok_or_else(|| parse_err("the lockfile is not a mapping".to_string()))?;
    let format = match root.get("lockfileVersion") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err(ExtractError::Unsupported("no lockfileVersion".to_string())),
    };
    let v9 = format.starts_with("9.");
    if !v9 && !format.starts_with("6.") {
        return Err(ExtractError::Unsupported(format!(
            "lockfileVersion {format}"
        )));
    }
    let lines = package_lines(text);
    let empty = Mapping::new();
    let section = |k: &str| -> Result<&Mapping, ExtractError> {
        match root.get(k) {
            None | Some(Value::Null) => Ok(&empty),
            Some(Value::Mapping(m)) => Ok(m),
            Some(_) => Err(parse_err(format!("`{k}` is not a mapping"))),
        }
    };
    let mut entries = Vec::new();

    // Members: the importers (a lone project has its dependencies at the top level).
    let importers = section("importers")?;
    let mut members: Vec<(String, &Value)> = importers
        .iter()
        .filter_map(|(k, v)| k.as_str().map(|k| (k.to_string(), v)))
        .collect();
    if importers.is_empty()
        && (root.contains_key("dependencies") || root.contains_key("devDependencies"))
    {
        members.push((".".to_string(), &doc));
    }
    for (key, v) in members {
        entries.push(LockEntry {
            name: if key == "." { "root".to_string() } else { key },
            version: String::new(),
            source: None,
            member: true,
            dev: false,
            deps: deps_of(
                v,
                &["dependencies", "devDependencies", "optionalDependencies"],
            ),
            lines: None,
        });
    }

    // External packages: variants differing only by a peer suffix are one entry.
    let packages = section("packages")?;
    let snapshots = section("snapshots")?;
    let mut merged: BTreeMap<(String, String), LockEntry> = BTreeMap::new();
    let mut errors = Vec::new();
    for (k, v) in packages {
        let Some(key) = k.as_str() else { continue };
        let Some((name, version)) = split_key(key) else {
            errors.push(parse_err(format!(
                "package key {key:?} is not name@version"
            )));
            continue;
        };
        if is_local(&version) {
            continue;
        }
        let e = merged
            .entry((name.clone(), version.clone()))
            .or_insert_with(|| LockEntry {
                name,
                version,
                source: None,
                member: false,
                dev: false,
                deps: Vec::new(),
                lines: lines.get(key).copied(),
            });
        if e.source.is_none() {
            e.source = v
                .get("resolution")
                .and_then(|r| r.get("tarball"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        e.dev |= v.get("dev").and_then(Value::as_bool) == Some(true);
        if !v9 {
            e.deps
                .extend(deps_of(v, &["dependencies", "optionalDependencies"]));
        }
    }
    if v9 {
        for (k, v) in snapshots {
            let Some((name, version)) = k.as_str().and_then(split_key) else {
                continue;
            };
            if let Some(e) = merged.get_mut(&(name, version)) {
                e.deps
                    .extend(deps_of(v, &["dependencies", "optionalDependencies"]));
            }
        }
    }
    for mut e in merged.into_values() {
        let set: BTreeSet<DepRef> = e.deps.drain(..).collect();
        e.deps = set.into_iter().collect();
        entries.push(e);
    }
    Ok(ParsedLock {
        namespace: "ts-package",
        ecosystem: "npm",
        format,
        entries,
        errors,
    })
}

#[cfg(test)]
const V9: &str = r#"lockfileVersion: '9.0'

settings:
  autoInstallPeers: true

importers:

  .:
    dependencies:
      alpha-lib:
        specifier: ^1.0.0
        version: 1.4.0
      '@scope/beta':
        specifier: ^2.0.0
        version: 2.0.0(alpha-lib@1.4.0)
    devDependencies:
      shared:
        specifier: workspace:*
        version: link:../shared

packages:

  '@scope/beta@2.0.0':
    resolution: {integrity: sha512-x, tarball: https://registry.example/beta-2.0.0.tgz}

  alpha-lib@1.4.0:
    resolution: {integrity: sha512-y}

  gamma@3.0.0:
    resolution: {integrity: sha512-z}

snapshots:

  '@scope/beta@2.0.0(alpha-lib@1.4.0)':
    dependencies:
      alpha-lib: 1.4.0
      gamma: 3.0.0

  alpha-lib@1.4.0: {}

  gamma@3.0.0: {}
"#;

#[cfg(test)]
const V6: &str = r#"lockfileVersion: '6.0'

dependencies:
  alpha-lib:
    specifier: ^1.0.0
    version: 1.4.0

packages:

  /alpha-lib@1.4.0:
    resolution: {integrity: sha512-y}
    dependencies:
      gamma: 3.0.0
    dev: false

  /gamma@3.0.0:
    resolution: {integrity: sha512-z}
    dev: true

  /delta@4.0.0(alpha-lib@1.4.0):
    resolution: {integrity: sha512-d}
    peerDependencies:
      alpha-lib: '*'
    dependencies:
      alpha-lib: 1.4.0
    dev: false

  /delta@4.0.0(alpha-lib@1.4.0)(react@18.0.0):
    resolution: {integrity: sha512-d}
    dependencies:
      alpha-lib: 1.4.0
      gamma: 3.0.0
    dev: false
"#;

#[cfg(test)]
#[test]
fn versions_become_nodes() {
    let (f, errors) = crate::run("pnpm-lock.yaml", V9).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    let versions: Vec<String> = crate::names(&f)
        .into_iter()
        .filter(|(_, k)| k == "package_version")
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        versions,
        vec![
            "app:pnpm-lock.yaml#package:@scope%2Fbeta/version:2.0.0",
            "app:pnpm-lock.yaml#package:alpha-lib/version:1.4.0",
            "app:pnpm-lock.yaml#package:gamma/version:3.0.0",
        ]
    );
    let beta = f
        .symbols
        .iter()
        .find(|s| s.name.segments[0].name == "@scope/beta" && s.kind == "package_version")
        .unwrap();
    assert_eq!(
        beta.facts.get("source"),
        Some(&katalogr_model::Fact::Str(
            "https://registry.example/beta-2.0.0.tgz".to_string()
        ))
    );
    assert_eq!((beta.span.start_line, beta.span.end_line), (23, 24));
    let exports: Vec<_> = f
        .exports
        .iter()
        .map(|e| (&*e.key.namespace, &*e.key.key))
        .collect();
    assert_eq!(exports.len(), 3);
    assert!(exports.iter().all(|(ns, _)| *ns == "ts-package"));

    let (f6, errors) = crate::run("pnpm-lock.yaml", V6).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    let versions: Vec<String> = crate::names(&f6)
        .into_iter()
        .filter(|(_, k)| k == "package_version")
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        versions,
        vec![
            "app:pnpm-lock.yaml#package:alpha-lib/version:1.4.0",
            "app:pnpm-lock.yaml#package:delta/version:4.0.0",
            "app:pnpm-lock.yaml#package:gamma/version:3.0.0",
        ]
    );
    let gamma = f6
        .symbols
        .iter()
        .find(|s| s.name.segments[0].name == "gamma" && s.kind == "package_version")
        .unwrap();
    assert_eq!(
        gamma.facts.get("dev"),
        Some(&katalogr_model::Fact::Bool(true))
    );
}

#[cfg(test)]
#[test]
fn peer_suffix_variants_are_one_version() {
    let (f, errors) = crate::run("pnpm-lock.yaml", V6).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    let delta: Vec<_> = f
        .symbols
        .iter()
        .filter(|s| s.name.segments[0].name == "delta" && s.kind == "package_version")
        .collect();
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].name.ordinal, 1);
    // Dependencies of both variants are unioned.
    let links = crate::link_lines(&f);
    assert!(links.contains(
        &"package:delta/version:4.0.0 depends-on package:alpha-lib/version:1.4.0".to_string()
    ));
    assert!(links.contains(
        &"package:delta/version:4.0.0 depends-on package:gamma/version:3.0.0".to_string()
    ));

    assert_eq!(
        split_key("/@s/n@1.0.0(react@18.0.0)"),
        Some(("@s/n".to_string(), "1.0.0".to_string()))
    );
    assert_eq!(
        split_key("n@1.0.0"),
        Some(("n".to_string(), "1.0.0".to_string()))
    );
    assert_eq!(split_key("@s/n"), None);
}

#[cfg(test)]
#[test]
fn importers_are_members() {
    let (f, _) = crate::run("pnpm-lock.yaml", V9).unwrap();
    let members: Vec<String> = f
        .symbols
        .iter()
        .filter(|s| s.kind == "package_member")
        .map(|s| s.name.segments[0].name.clone())
        .collect();
    assert_eq!(members, vec!["root"]);
    assert!(f.exports.iter().all(|e| e.key.key != "root"));
    let links = crate::link_lines(&f);
    assert!(links.contains(&"member:root depends-on package:alpha-lib/version:1.4.0".to_string()));
    // A `link:` version is a local package, so it gets no external link.
    assert_eq!(
        links
            .iter()
            .filter(|l| l.starts_with("member:root"))
            .count(),
        2
    );

    let text = "lockfileVersion: '9.0'\n\nimporters:\n\n  .:\n    dependencies: {}\n\n  packages/inner:\n    dependencies: {}\n";
    let (f, _) = crate::run("pnpm-lock.yaml", text).unwrap();
    let members: Vec<String> = f
        .symbols
        .iter()
        .filter(|s| s.kind == "package_member")
        .map(|s| s.name.segments[0].name.clone())
        .collect();
    assert_eq!(members, vec!["packages/inner", "root"]);
}

#[cfg(test)]
#[test]
fn snapshots_link_versions() {
    let (f, errors) = crate::run("pnpm-lock.yaml", V9).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        crate::link_lines(&f),
        vec![
            "member:root depends-on package:@scope%2Fbeta/version:2.0.0",
            "member:root depends-on package:alpha-lib/version:1.4.0",
            "package:@scope%2Fbeta has-version package:@scope%2Fbeta/version:2.0.0",
            "package:@scope%2Fbeta/version:2.0.0 depends-on package:alpha-lib/version:1.4.0",
            "package:@scope%2Fbeta/version:2.0.0 depends-on package:gamma/version:3.0.0",
            "package:alpha-lib has-version package:alpha-lib/version:1.4.0",
            "package:gamma has-version package:gamma/version:3.0.0",
        ]
    );
}

#[cfg(test)]
#[test]
fn unknown_version_is_unsupported() {
    for text in [
        "lockfileVersion: '5.4'\n",
        "lockfileVersion: 5.4\n",
        "lockfileVersion: '10.0'\n",
        "packages: {}\n",
    ] {
        let err = crate::run("pnpm-lock.yaml", text).err().unwrap();
        assert!(
            matches!(err, ExtractError::Unsupported(_)),
            "{text}: {err:?}"
        );
    }
    assert!(crate::run("pnpm-lock.yaml", "lockfileVersion: '6.0'\n").is_ok());
    // Empty and truncated input are errors, never panics.
    assert!(crate::run("pnpm-lock.yaml", "").is_err());
    assert!(
        crate::run(
            "pnpm-lock.yaml",
            "lockfileVersion: '9.0'\npackages:\n  'a@1': {"
        )
        .is_err()
    );
}
