//! `Cargo.lock`: each `[[package]]` becomes an entry (FEAT-007 R-1, R-4, R-5).

use crate::lock_common::{DepRef, LockEntry, ParsedLock};
use katalogr_model::ExtractError;
use toml::{Table, Value};

fn parse_err(message: String) -> ExtractError {
    ExtractError::Parse {
        span: None,
        message,
    }
}

/// `name`, `name version` or `name version (source)`.
fn dep_ref(s: &str) -> DepRef {
    let mut parts = s.split_whitespace();
    let name = parts.next().unwrap_or("").to_string();
    let version = parts
        .next()
        .filter(|v| !v.starts_with('('))
        .map(str::to_string);
    DepRef { name, version }
}

/// The 1-based `(start, end)` lines of each `[[package]]` table, in file order: from its header
/// to the line before the next header, the last to the end of the file.
fn package_lines(text: &str) -> Vec<(u32, u32)> {
    let mut starts: Vec<u32> = Vec::new();
    let mut total = 0u32;
    for (i, line) in text.split('\n').enumerate() {
        total = i as u32 + 1;
        if line.trim_end() == "[[package]]" {
            starts.push(total);
        }
    }
    if text.ends_with('\n') {
        total = total.saturating_sub(1);
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let end = starts.get(i + 1).map_or(total, |n| n - 1);
            (*s, end.max(*s))
        })
        .collect()
}

pub fn parse(text: &str) -> Result<ParsedLock, ExtractError> {
    let doc: Table = toml::from_str(text).map_err(|e| parse_err(e.to_string()))?;
    let format = match doc.get("version") {
        None => "1".to_string(),
        Some(Value::Integer(n)) if (1..=4).contains(n) => n.to_string(),
        Some(Value::Integer(n)) => {
            return Err(ExtractError::Unsupported(format!("Cargo.lock version {n}")));
        }
        Some(other) => {
            return Err(parse_err(format!(
                "Cargo.lock version is not a number: {other}"
            )));
        }
    };
    let packages: &[Value] = match doc.get("package") {
        None => &[],
        Some(Value::Array(a)) => a,
        Some(_) => return Err(parse_err("`package` is not an array of tables".to_string())),
    };
    let lines = package_lines(text);
    // Lines are only trusted when the scan found one header per parsed table.
    let lines_ok = lines.len() == packages.len();
    let mut entries = Vec::with_capacity(packages.len());
    for (i, p) in packages.iter().enumerate() {
        let Some(t) = p.as_table() else {
            return Err(parse_err("a `package` entry is not a table".to_string()));
        };
        let text_of = |k: &str| t.get(k).and_then(Value::as_str).map(str::to_string);
        let (Some(name), Some(version)) = (text_of("name"), text_of("version")) else {
            return Err(parse_err(
                "a `package` entry has no name or version".to_string(),
            ));
        };
        let source = text_of("source");
        let deps = t
            .get("dependencies")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(dep_ref).collect())
            .unwrap_or_default();
        entries.push(LockEntry {
            name,
            version,
            member: source.is_none(),
            source,
            dev: false,
            deps,
            lines: if lines_ok {
                lines.get(i).copied()
            } else {
                None
            },
        });
    }
    Ok(ParsedLock {
        namespace: "rs-package",
        ecosystem: "cargo",
        format,
        entries,
        errors: Vec::new(),
    })
}

#[cfg(test)]
const TEXT: &str = r#"# generated
version = 3

[[package]]
name = "alpha-lib"
version = "1.2.3"
source = "registry+https://registry.example/index"
dependencies = [
 "beta-lib",
]

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
 "alpha-lib 1.2.3 (registry+https://registry.example/index)",
 "beta-lib 2.0.0",
]

[[package]]
name = "beta-lib"
version = "2.0.0"
source = "registry+https://registry.example/index"
"#;

#[cfg(test)]
#[test]
fn versions_become_nodes() {
    let (f, errors) = crate::run("Cargo.lock", TEXT).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    let n = crate::names(&f);
    let n: Vec<(&str, &str)> = n.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    assert_eq!(
        n,
        vec![
            ("app:Cargo.lock#member:app", "package_member"),
            ("app:Cargo.lock#package:alpha-lib", "package_external"),
            (
                "app:Cargo.lock#package:alpha-lib/version:1.2.3",
                "package_version"
            ),
            ("app:Cargo.lock#package:beta-lib", "package_external"),
            (
                "app:Cargo.lock#package:beta-lib/version:2.0.0",
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
        vec![("rs-package", "alpha-lib"), ("rs-package", "beta-lib")]
    );
    // The version node spans its table.
    let alpha = f
        .symbols
        .iter()
        .find(|s| s.kind == "package_version" && s.name.segments[0].name == "alpha-lib")
        .unwrap();
    assert_eq!((alpha.span.start_line, alpha.span.end_line), (4, 11));
}

#[cfg(test)]
#[test]
fn two_versions_two_nodes() {
    let text = r#"version = 3

[[package]]
name = "alpha-lib"
version = "1.0.0"
source = "registry+https://r.example/"

[[package]]
name = "alpha-lib"
version = "2.0.0"
source = "registry+https://r.example/"
"#;
    let (f, _) = crate::run("Cargo.lock", text).unwrap();
    let versions: Vec<String> = crate::names(&f)
        .into_iter()
        .filter(|(_, k)| k == "package_version")
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        versions,
        vec![
            "app:Cargo.lock#package:alpha-lib/version:1.0.0",
            "app:Cargo.lock#package:alpha-lib/version:2.0.0"
        ]
    );
    // One name node, with two versions under it.
    assert_eq!(f.exports.len(), 1);
    assert_eq!(
        crate::link_lines(&f),
        vec![
            "package:alpha-lib has-version package:alpha-lib/version:1.0.0",
            "package:alpha-lib has-version package:alpha-lib/version:2.0.0",
        ]
    );
}

#[cfg(test)]
#[test]
fn same_version_two_sources_get_ordinals() {
    let text = r#"version = 3

[[package]]
name = "alpha-lib"
version = "1.0.0"
source = "registry+https://r.example/"

[[package]]
name = "alpha-lib"
version = "1.0.0"
source = "git+https://g.example/alpha?rev=abc#abc"
"#;
    let (f, _) = crate::run("Cargo.lock", text).unwrap();
    let versions: Vec<String> = crate::names(&f)
        .into_iter()
        .filter(|(_, k)| k == "package_version")
        .map(|(n, _)| n)
        .collect();
    // Sorted by source: `git+…` before `registry+…`.
    assert_eq!(
        versions,
        vec![
            "app:Cargo.lock#package:alpha-lib/version:1.0.0",
            "app:Cargo.lock#package:alpha-lib/version:1.0.0~2"
        ]
    );
    let first = f
        .symbols
        .iter()
        .find(|s| s.name.ordinal == 1 && s.kind == "package_version");
    let source = first.unwrap().facts.get("source");
    assert_eq!(
        source,
        Some(&katalogr_model::Fact::Str(
            "git+https://g.example/alpha?rev=abc#abc".to_string()
        ))
    );
}

#[cfg(test)]
#[test]
fn local_entry_is_a_member() {
    let (f, _) = crate::run("Cargo.lock", TEXT).unwrap();
    let member = f
        .symbols
        .iter()
        .find(|s| s.kind == "package_member")
        .unwrap();
    assert_eq!(member.name.segments[0].name, "app");
    assert_eq!(
        member.facts.get("version"),
        Some(&katalogr_model::Fact::Str("0.1.0".to_string()))
    );
    // A member exports nothing: its manifest already does.
    assert!(f.exports.iter().all(|e| e.key.key != "app"));
    assert_eq!(
        f.symbols
            .iter()
            .filter(|s| s.kind == "package_member")
            .count(),
        1
    );
}

#[cfg(test)]
#[test]
fn dependencies_link_versions() {
    let (f, errors) = crate::run("Cargo.lock", TEXT).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        crate::link_lines(&f),
        vec![
            "member:app depends-on package:alpha-lib/version:1.2.3",
            "member:app depends-on package:beta-lib/version:2.0.0",
            "package:alpha-lib has-version package:alpha-lib/version:1.2.3",
            "package:alpha-lib/version:1.2.3 depends-on package:beta-lib/version:2.0.0",
            "package:beta-lib has-version package:beta-lib/version:2.0.0",
        ]
    );
    // A dependency naming no entry is recoverable.
    let text = "version = 3\n\n[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nsource = \"registry+https://r.example/\"\ndependencies = [\"ghost\"]\n";
    let (f, errors) = crate::run("Cargo.lock", text).unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(f.links.len(), 1);
}

#[cfg(test)]
#[test]
fn unknown_version_is_unsupported() {
    let err = crate::run("Cargo.lock", "version = 5\n").err().unwrap();
    assert!(matches!(err, ExtractError::Unsupported(_)), "{err:?}");
    assert!(crate::run("Cargo.lock", "version = 4\n").is_ok());
    assert!(crate::run("Cargo.lock", "").is_ok());
    // Truncated input is an error, never a panic.
    assert!(crate::run("Cargo.lock", "[[package]]\nname = \"a").is_err());
}
