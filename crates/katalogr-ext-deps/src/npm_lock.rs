//! `package-lock.json`, from its `packages` map (FEAT-007 R-2, R-4, R-5).

use crate::lock_common::{DepRef, LockEntry, ParsedLock};
use katalogr_model::ExtractError;
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};

fn parse_err(message: String) -> ExtractError {
    ExtractError::Parse {
        span: None,
        message,
    }
}

const NM: &str = "node_modules/";

/// The text after the last `node_modules/` segment of `key`, if `key` ends in one.
fn external_name(key: &str) -> Option<&str> {
    let i = if key.starts_with(NM) {
        0
    } else {
        key.rfind("/node_modules/")? + 1
    };
    let rest = key[i..].rsplit_once(NM)?.1;
    // A name is one segment, or `@scope/name`; anything deeper is a path inside a package.
    let slashes = rest.matches('/').count();
    let ok = match slashes {
        0 => true,
        1 => rest.starts_with('@'),
        _ => false,
    };
    Some(rest).filter(|r| !r.is_empty() && ok)
}

/// Resolve dependency `dep` of the entry at `key`: try `<key>/node_modules/<dep>`, then remove
/// one trailing `/node_modules/<name>` from `key` at a time, down to `node_modules/<dep>`.
/// A member's own path is tried first and then the root. Returns the key that exists.
pub fn resolve_nested(path: &str, dep: &str, keys: &BTreeSet<&str>) -> Option<String> {
    let mut base = path.to_string();
    loop {
        let candidate = if base.is_empty() {
            format!("{NM}{dep}")
        } else {
            format!("{base}/{NM}{dep}")
        };
        if keys.contains(candidate.as_str()) {
            return Some(candidate);
        }
        if base.is_empty() {
            return None;
        }
        // Remove one trailing `/node_modules/<name>`; a path with none is a member's own.
        base = match base.rfind("/node_modules/") {
            Some(i) => base[..i].to_string(),
            None => String::new(),
        };
    }
}

/// `"<key>": {` headers at four spaces of indent, with the first line of the entry and the
/// line of its closing brace. One pass over the text, so the whole file stays linear.
fn entry_lines(text: &str) -> HashMap<&str, (u32, u32)> {
    let mut out: HashMap<&str, (u32, u32)> = HashMap::new();
    let mut open: Option<(&str, u32)> = None;
    for (i, line) in text.split('\n').enumerate() {
        let n = i as u32 + 1;
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line == "    }" || line == "    }," {
            if let Some((header, start)) = open.take() {
                out.entry(header).or_insert((start, n));
            }
        } else if line.starts_with("    \"") && line.ends_with(": {") {
            open = Some((line, n));
        }
    }
    out
}

pub fn parse(text: &str) -> Result<ParsedLock, ExtractError> {
    let doc: Value = serde_json::from_str(text).map_err(|e| parse_err(e.to_string()))?;
    let root = doc
        .as_object()
        .ok_or_else(|| parse_err("the lockfile is not an object".to_string()))?;
    let format = match root.get("lockfileVersion").and_then(Value::as_i64) {
        Some(v @ (2 | 3)) => v.to_string(),
        Some(v) => return Err(ExtractError::Unsupported(format!("lockfileVersion {v}"))),
        None => return Err(ExtractError::Unsupported("no lockfileVersion".to_string())),
    };
    let empty = Map::new();
    let packages = match root.get("packages") {
        None => &empty,
        Some(Value::Object(m)) => m,
        Some(_) => return Err(parse_err("`packages` is not an object".to_string())),
    };
    let keys: BTreeSet<&str> = packages.keys().map(String::as_str).collect();
    let lines = entry_lines(text);
    let str_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let is_member = |key: &str| key.is_empty() || !key.contains(NM);
    // The name a package entry has, for a dependency to name it.
    let name_of = |key: &str, v: &Value| -> String {
        if let Some(n) = str_of(v, "name") {
            return n;
        }
        if is_member(key) {
            return match key.rsplit('/').next() {
                Some(l) if !l.is_empty() => l.to_string(),
                _ => "root".to_string(),
            };
        }
        external_name(key).unwrap_or(key).to_string()
    };

    let mut entries = Vec::new();
    let mut errors = Vec::new();
    for (key, v) in packages {
        if v.get("link").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let member = is_member(key);
        if !member && external_name(key).is_none() {
            continue;
        }
        let header = format!("    {}: {{", serde_json::to_string(key).unwrap_or_default());
        let span = lines.get(header.as_str()).copied();
        let mut deps = Vec::new();
        for (field, soft) in [
            ("dependencies", false),
            ("devDependencies", false),
            ("optionalDependencies", true),
            ("peerDependencies", true),
        ] {
            let Some(m) = v.get(field).and_then(Value::as_object) else {
                continue;
            };
            for dep in m.keys() {
                match resolve_nested(key, dep, &keys) {
                    Some(k) => {
                        let target = &packages[k.as_str()];
                        if target.get("link").and_then(Value::as_bool) == Some(true) {
                            // A workspace link names a member, which is not an external version.
                            let to = target
                                .get("resolved")
                                .and_then(Value::as_str)
                                .and_then(|r| packages.get(r).map(|t| (r, t)));
                            deps.push(match to {
                                Some((r, t)) => DepRef {
                                    name: name_of(r, t),
                                    version: str_of(t, "version").filter(|s| !s.is_empty()),
                                },
                                None => DepRef {
                                    name: dep.clone(),
                                    version: None,
                                },
                            });
                        } else {
                            deps.push(DepRef {
                                name: name_of(&k, target),
                                version: str_of(target, "version"),
                            });
                        }
                    }
                    None if soft => {}
                    None => errors.push(ExtractError::Parse {
                        span: span.map(|(a, b)| katalogr_model::Span {
                            start_line: a,
                            start_col: 1,
                            end_line: b,
                            end_col: 1,
                        }),
                        message: format!("dependency {dep:?} of {key:?} names no entry"),
                    }),
                }
            }
        }
        deps.sort();
        deps.dedup();
        entries.push(LockEntry {
            name: name_of(key, v),
            version: str_of(v, "version").unwrap_or_default(),
            source: if member { None } else { str_of(v, "resolved") },
            member,
            dev: v.get("dev").and_then(Value::as_bool) == Some(true),
            deps,
            lines: span,
        });
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
const TEXT: &str = r#"{
  "name": "app",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {
    "": {
      "name": "app",
      "version": "1.0.0",
      "dependencies": {
        "alpha-lib": "^1.0.0",
        "@scope/beta": "^2.0.0"
      },
      "devDependencies": {
        "gamma-dev": "^3.0.0"
      }
    },
    "node_modules/@scope/beta": {
      "version": "2.0.0",
      "resolved": "https://registry.example/@scope/beta/-/beta-2.0.0.tgz",
      "dependencies": {
        "alpha-lib": "^2.0.0"
      }
    },
    "node_modules/@scope/beta/node_modules/alpha-lib": {
      "version": "2.1.0",
      "resolved": "https://registry.example/alpha-lib/-/alpha-lib-2.1.0.tgz"
    },
    "node_modules/alpha-lib": {
      "version": "1.4.0",
      "resolved": "https://registry.example/alpha-lib/-/alpha-lib-1.4.0.tgz",
      "peerDependencies": {
        "missing-peer": "*"
      }
    },
    "node_modules/gamma-dev": {
      "version": "3.0.0",
      "resolved": "https://registry.example/gamma-dev/-/gamma-dev-3.0.0.tgz",
      "dev": true
    }
  }
}
"#;

#[cfg(test)]
#[test]
fn versions_become_nodes() {
    let (f, errors) = crate::run("package-lock.json", TEXT).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    let n = crate::names(&f);
    let n: Vec<(&str, &str)> = n.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    assert_eq!(
        n,
        vec![
            ("app:package-lock.json#member:app", "package_member"),
            (
                "app:package-lock.json#package:@scope%2Fbeta",
                "package_external"
            ),
            (
                "app:package-lock.json#package:@scope%2Fbeta/version:2.0.0",
                "package_version"
            ),
            (
                "app:package-lock.json#package:alpha-lib",
                "package_external"
            ),
            (
                "app:package-lock.json#package:alpha-lib/version:1.4.0",
                "package_version"
            ),
            (
                "app:package-lock.json#package:alpha-lib/version:2.1.0",
                "package_version"
            ),
            (
                "app:package-lock.json#package:gamma-dev",
                "package_external"
            ),
            (
                "app:package-lock.json#package:gamma-dev/version:3.0.0",
                "package_version"
            ),
        ]
    );
    let exports: Vec<_> = f
        .exports
        .iter()
        .map(|e| (&*e.key.namespace, &*e.key.key))
        .collect();
    assert_eq!(
        exports,
        vec![
            ("ts-package", "@scope/beta"),
            ("ts-package", "alpha-lib"),
            ("ts-package", "gamma-dev")
        ]
    );
    let gamma = f
        .symbols
        .iter()
        .find(|s| s.kind == "package_version" && s.name.segments[0].name == "gamma-dev")
        .unwrap();
    assert_eq!(
        gamma.facts.get("dev"),
        Some(&katalogr_model::Fact::Bool(true))
    );
    // The entry's own lines: from its header to its closing brace.
    assert_eq!((gamma.span.start_line, gamma.span.end_line), (36, 40));
}

#[cfg(test)]
#[test]
fn workspace_entries_are_members() {
    let text = r#"{
  "lockfileVersion": 3,
  "packages": {
    "": {
      "workspaces": ["packages/*"]
    },
    "node_modules/inner": {
      "resolved": "packages/inner",
      "link": true
    },
    "node_modules/zeta": {
      "version": "1.0.0",
      "resolved": "https://registry.example/zeta-1.0.0.tgz"
    },
    "packages/inner": {
      "name": "inner",
      "version": "0.3.0",
      "dependencies": {
        "zeta": "^1.0.0"
      }
    }
  }
}
"#;
    let (f, errors) = crate::run("package-lock.json", text).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    let members: Vec<String> = f
        .symbols
        .iter()
        .filter(|s| s.kind == "package_member")
        .map(|s| s.name.segments[0].name.clone())
        .collect();
    // The unnamed root is `root`; the link entry is skipped, so `inner` appears once.
    assert_eq!(members, vec!["inner", "root"]);
    assert!(
        f.exports
            .iter()
            .all(|e| e.key.key != "inner" && e.key.key != "root")
    );
    assert_eq!(
        crate::link_lines(&f),
        vec![
            "member:inner depends-on package:zeta/version:1.0.0",
            "package:zeta has-version package:zeta/version:1.0.0",
        ]
    );
}

#[cfg(test)]
#[test]
fn nested_resolution_links_versions() {
    let (f, errors) = crate::run("package-lock.json", TEXT).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    let links = crate::link_lines(&f);
    // The nested copy wins for the package that holds it; the root copy for everyone else.
    assert!(
        links.contains(
            &"package:@scope%2Fbeta/version:2.0.0 depends-on package:alpha-lib/version:2.1.0"
                .to_string()
        )
    );
    assert!(links.contains(&"member:app depends-on package:alpha-lib/version:1.4.0".to_string()));
    assert!(links.contains(&"member:app depends-on package:gamma-dev/version:3.0.0".to_string()));

    let keys: BTreeSet<&str> = [
        "node_modules/a/node_modules/b/node_modules/c",
        "node_modules/a/node_modules/d",
        "node_modules/e",
        "pkgs/m/node_modules/e",
    ]
    .into_iter()
    .collect();
    let r = |p: &str, d: &str| resolve_nested(p, d, &keys);
    assert_eq!(
        r("node_modules/a/node_modules/b", "c").as_deref(),
        Some("node_modules/a/node_modules/b/node_modules/c")
    );
    assert_eq!(
        r("node_modules/a/node_modules/b", "d").as_deref(),
        Some("node_modules/a/node_modules/d")
    );
    assert_eq!(
        r("node_modules/a/node_modules/b", "e").as_deref(),
        Some("node_modules/e")
    );
    assert_eq!(r("pkgs/m", "e").as_deref(), Some("pkgs/m/node_modules/e"));
    assert_eq!(r("pkgs/other", "e").as_deref(), Some("node_modules/e"));
    assert_eq!(r("", "zzz"), None);
}

#[cfg(test)]
#[test]
fn version_one_is_unsupported() {
    for text in [
        r#"{"lockfileVersion": 1, "dependencies": {}}"#,
        r#"{"packages": {}}"#,
        r#"{"lockfileVersion": 4, "packages": {}}"#,
    ] {
        let err = crate::run("package-lock.json", text).err().unwrap();
        assert!(
            matches!(err, ExtractError::Unsupported(_)),
            "{text}: {err:?}"
        );
    }
    assert!(
        crate::run(
            "package-lock.json",
            r#"{"lockfileVersion": 2, "packages": {}}"#
        )
        .is_ok()
    );
    // Empty and truncated input are errors, never panics.
    assert!(crate::run("package-lock.json", "").is_err());
    assert!(crate::run("package-lock.json", "{\"lockfileVersion\": 3, \"pack").is_err());
}
