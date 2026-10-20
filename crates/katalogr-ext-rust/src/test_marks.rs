//! Test marks for the neutral `test` fact (FEAT-063 section 6.1, ADR-0018): `case` for test
//! functions, `suite` for test modules and test files, `support` for the rest of their code, and
//! the `test-name` exports of the cases.

use katalogr_model::insight::{
    FACT_TEST, TEST_CASE, TEST_NAME_NS, TEST_SUITE, TEST_SUPPORT, test_name_keys,
};
use katalogr_model::{Export, ExportKey, Fact, Fragment, Name, Segment};
use std::collections::BTreeSet;

/// The attribute's path: the text before any `(`, `=` or space.
fn path_of(attr: &str) -> &str {
    let a = attr.trim();
    let end = a.find(['(', '=', ' ']).unwrap_or(a.len());
    &a[..end]
}

/// True for an attribute text (as `attr_texts` records it) that marks a test function: the path
/// before any `(` is `test`, or ends with `::test` (`tokio::test`, `async_std::test`).
pub(crate) fn is_test_attr(attr: &str) -> bool {
    let p = path_of(attr);
    p == "test" || p.ends_with("::test")
}

/// True for `cfg(...)` whose predicate contains the identifier `test` and no `not(test`
/// (spaces removed first): `cfg(test)`, `cfg(all(test, unix))`; not `cfg(not(test))`, not
/// `cfg(tests)`, not `cfg(feature = "test")`.
pub(crate) fn is_cfg_test(attr: &str) -> bool {
    let a: String = attr.chars().filter(|c| !c.is_whitespace()).collect();
    let Some(inner) = a.strip_prefix("cfg(").and_then(|r| r.strip_suffix(')')) else {
        return false;
    };
    if inner.contains("not(test") {
        return false;
    }
    // Drop string literals, then look for `test` as a whole identifier.
    let mut plain = String::new();
    let mut in_str = false;
    for c in inner.chars() {
        if c == '"' {
            in_str = !in_str;
        } else if !in_str {
            plain.push(c);
        }
    }
    plain
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| w == "test")
}

fn attrs_of(facts: &std::collections::BTreeMap<String, Fact>) -> Vec<&str> {
    match facts.get("attrs") {
        Some(Fact::List(l)) => l
            .iter()
            .filter_map(|f| match f {
                Fact::Str(s) => Some(s.as_str()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Sets `test` facts on the fragment's symbols and artifact and pushes the `test-name` exports
/// of its cases. `module` is the file's module path from its target root (`Owner::module`);
/// `test_target` is `Some(target name)` when the owner target kind is `Test`.
pub(crate) fn mark(fragment: &mut Fragment, module: &[String], test_target: Option<&str>) {
    let in_test_file = test_target.is_some();
    let mut suites: BTreeSet<Vec<Segment>> = BTreeSet::new();
    for s in &fragment.symbols {
        if s.kind == "mod" && (in_test_file || attrs_of(&s.facts).iter().any(|a| is_cfg_test(a))) {
            suites.insert(s.name.segments.clone());
        }
    }
    let mut cases: Vec<(katalogr_model::Id, Vec<String>)> = Vec::new();
    for s in &mut fragment.symbols {
        let segs = &s.name.segments;
        let role = if s.kind == "fn" && attrs_of(&s.facts).iter().any(|a| is_test_attr(a)) {
            let mut path: Vec<String> = Vec::new();
            if let Some(t) = test_target {
                path.push(t.to_string());
            }
            path.extend(module.iter().cloned());
            path.extend(segs.iter().map(|g| g.name.clone()));
            cases.push((s.name.id(), path));
            Some(TEST_CASE)
        } else if s.kind == "mod" && suites.contains(segs) {
            Some(TEST_SUITE)
        } else if in_test_file || (1..segs.len()).any(|n| suites.contains(&segs[..n])) {
            Some(TEST_SUPPORT)
        } else {
            None
        };
        if let Some(r) = role {
            s.facts
                .insert(FACT_TEST.to_string(), Fact::Str(r.to_string()));
        }
    }
    if in_test_file {
        fragment
            .artifact
            .facts
            .insert(FACT_TEST.to_string(), Fact::Str(TEST_SUITE.to_string()));
    }
    export_names(fragment, &cases);
}

/// One `test-name` export per key of each case's runner path.
fn export_names(fragment: &mut Fragment, cases: &[(katalogr_model::Id, Vec<String>)]) {
    for (id, path) in cases {
        let refs: Vec<&str> = path.iter().map(String::as_str).collect();
        for key in test_name_keys(&refs) {
            fragment.exports.push(Export {
                key: ExportKey {
                    namespace: TEST_NAME_NS.to_string(),
                    key,
                },
                target: *id,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attr_paths() {
        assert!(is_test_attr("test"));
        assert!(is_test_attr("tokio::test(flavor = \"multi_thread\")"));
        assert!(is_test_attr("async_std::test"));
        assert!(!is_test_attr("testing"));
        assert!(!is_test_attr("rstest"));
    }

    #[test]
    fn cfg_test_predicates() {
        assert!(is_cfg_test("cfg(test)"));
        assert!(is_cfg_test("cfg(all(test, unix))"));
        assert!(!is_cfg_test("cfg(not(test))"));
        assert!(!is_cfg_test("cfg(tests)"));
        assert!(!is_cfg_test("cfg(feature = \"test\")"));
    }
}
