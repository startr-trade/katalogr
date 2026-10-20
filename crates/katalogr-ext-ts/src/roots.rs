//! Entry-point reasons for the `root` fact and the package of a file (FEAT-030 section 5,
//! ADR-0018). The first rule that matches wins; the only weak reason is `api`.

use crate::context::TsContext;
use katalogr_model::insight::{ROOT_API, ROOT_IMPL, ROOT_TEST};

/// Symbol kinds that are code, hence dead-code candidates and roots.
pub const CODE_KINDS: &[&str] = &[
    "fn",
    "class",
    "method",
    "interface",
    "type",
    "enum",
    "const",
];

/// What the rules look at for one symbol.
#[derive(Clone, Copy, Debug)]
pub struct RootCtx<'a> {
    pub kind: &'a str,
    /// The file is test code (see [`is_test_path`]).
    pub test_file: bool,
    /// The `exported` fact.
    pub exported: bool,
    /// A `method` of a class that implements or extends something.
    pub in_derived_class: bool,
}

/// `*.test.*`, `*.spec.*`, or a file below a `__tests__`, `test` or `tests` directory.
pub fn is_test_path(path: &str) -> bool {
    crate::tests::is_test_file(path)
        || path
            .split('/')
            .rev()
            .skip(1)
            .any(|d| matches!(d, "test" | "tests"))
}

/// Why the symbol is an entry point, if it is.
pub fn root_reason(c: &RootCtx) -> Option<&'static str> {
    if !CODE_KINDS.contains(&c.kind) {
        return None;
    }
    if c.test_file {
        Some(ROOT_TEST)
    } else if c.exported {
        Some(ROOT_API)
    } else if c.kind == "method" && c.in_derived_class {
        Some(ROOT_IMPL)
    } else {
        None
    }
}

/// True when `path` is the entry file of a project (`main`, else `exports`).
pub fn is_entry(ctx: &TsContext, path: &str) -> bool {
    ctx.projects.iter().any(|p| {
        p.entry
            .as_deref()
            .is_some_and(|e| e.trim_start_matches("./") == path)
    })
}

/// The `name` of the `package.json` of the innermost project whose directory holds `path`.
pub fn package_of<'c>(ctx: &'c TsContext, path: &str) -> Option<&'c str> {
    ctx.projects
        .iter()
        .filter(|p| p.package_name.is_some())
        .filter(|p| {
            p.dir.is_empty()
                || path
                    .strip_prefix(p.dir.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        })
        .max_by_key(|p| p.dir.len())
        .and_then(|p| p.package_name.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paths_and_reasons() {
        assert!(is_test_path("src/a.test.ts"));
        assert!(is_test_path("src/a.spec.js"));
        assert!(is_test_path("src/__tests__/a.ts"));
        assert!(is_test_path("pkg/test/a.ts"));
        assert!(!is_test_path("src/contest.ts"));
        assert!(!is_test_path("src/tests.ts"));
        let c = RootCtx {
            kind: "method",
            test_file: false,
            exported: false,
            in_derived_class: true,
        };
        assert_eq!(root_reason(&c), Some("impl"));
        assert_eq!(
            root_reason(&RootCtx {
                exported: true,
                ..c
            }),
            Some("api")
        );
        assert_eq!(
            root_reason(&RootCtx {
                test_file: true,
                ..c
            }),
            Some("test")
        );
        assert_eq!(
            root_reason(&RootCtx {
                kind: "reexport",
                ..c
            }),
            None
        );
    }
}
