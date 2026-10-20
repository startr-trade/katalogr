//! Entry-point reasons for the `root` fact (FEAT-030 section 5, ADR-0018). The first rule that
//! matches wins; the only weak reason is `api`.

use crate::context::TargetKind;
use katalogr_model::insight::{ROOT_API, ROOT_EXTERNAL, ROOT_IMPL, ROOT_MAIN, ROOT_TEST};

/// Symbol kinds that are code, hence dead-code candidates and roots.
pub const CODE_KINDS: &[&str] = &[
    "fn", "struct", "enum", "union", "trait", "const", "static", "type", "macro",
];

/// What the rules look at for one symbol.
#[derive(Clone, Copy, Debug)]
pub struct RootCtx<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    /// Attribute texts without `#[` and `]`.
    pub attrs: &'a [String],
    /// The `vis` fact (`pub`, `pub(crate)`, `private`).
    pub vis: Option<&'a str>,
    /// Kind of the target that owns the file.
    pub target: Option<TargetKind>,
    /// The symbol is at the top level of a target's root file.
    pub at_target_root: bool,
    /// Inside a module (or file) carrying `#[cfg(test)]`.
    pub in_test_code: bool,
    /// A `fn` directly inside `impl Trait for Type`.
    pub in_trait_impl: bool,
    /// The `fn` has an `extern "..."` ABI.
    pub extern_abi: bool,
}

/// The attribute's path: `tokio::test` for `tokio::test(flavor = "x")`; `unsafe(no_mangle)`
/// is read as `no_mangle`.
fn attr_path(attr: &str) -> &str {
    let a = attr.trim();
    let a = a
        .strip_prefix("unsafe(")
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or(a)
        .trim();
    let end = a.find(['(', '=', ' ']).unwrap_or(a.len());
    &a[..end]
}

/// True for a `cfg` attribute that requires `test` (shared with the `test` fact, FEAT-063).
pub fn is_cfg_test(attr: &str) -> bool {
    crate::test_marks::is_cfg_test(attr)
}

fn path_is(path: &str, word: &str) -> bool {
    path == word || path.ends_with(&format!("::{word}"))
}

/// Why the symbol is an entry point, if it is.
pub fn root_reason(c: &RootCtx) -> Option<&'static str> {
    if !CODE_KINDS.contains(&c.kind) {
        return None;
    }
    let is_main = c.kind == "fn" && c.name == "main" && c.at_target_root;
    let paths: Vec<&str> = c.attrs.iter().map(|a| attr_path(a)).collect();
    let test_file = match c.target {
        Some(TargetKind::Test | TargetKind::Bench) => true,
        Some(TargetKind::Example) => !is_main,
        _ => false,
    };
    // 1. Test, bench or test-only code.
    if paths
        .iter()
        .any(|p| crate::test_marks::is_test_attr(p) || path_is(p, "bench"))
        || c.in_test_code
        || test_file
    {
        return Some(ROOT_TEST);
    }
    // 2. Program entry.
    if is_main && matches!(c.target, Some(TargetKind::Bin | TargetKind::Example)) {
        return Some(ROOT_MAIN);
    }
    // 3. Called from outside the language.
    const EXTERNAL: [&str; 5] = [
        "no_mangle",
        "export_name",
        "proc_macro",
        "proc_macro_derive",
        "proc_macro_attribute",
    ];
    if paths.iter().any(|p| EXTERNAL.contains(p)) || c.extern_abi {
        return Some(ROOT_EXTERNAL);
    }
    // 4. Reached through trait dispatch.
    if c.kind == "fn" && c.in_trait_impl {
        return Some(ROOT_IMPL);
    }
    // 5. Public API of a library.
    if c.vis == Some("pub") && c.target == Some(TargetKind::Lib) {
        return Some(ROOT_API);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(kind: &'a str, name: &'a str, attrs: &'a [String]) -> RootCtx<'a> {
        RootCtx {
            kind,
            name,
            attrs,
            vis: Some("private"),
            target: Some(TargetKind::Lib),
            at_target_root: true,
            in_test_code: false,
            in_trait_impl: false,
            extern_abi: false,
        }
    }

    #[test]
    fn rules_in_order() {
        let none: Vec<String> = Vec::new();
        assert_eq!(root_reason(&ctx("fn", "f", &none)), None);
        let t = vec!["tokio::test(flavor = \"x\")".to_string()];
        assert_eq!(root_reason(&ctx("fn", "f", &t)), Some("test"));
        let u = vec!["unsafe(no_mangle)".to_string()];
        assert_eq!(root_reason(&ctx("fn", "f", &u)), Some("external"));
        let mut c = ctx("fn", "main", &none);
        c.target = Some(TargetKind::Example);
        assert_eq!(root_reason(&c), Some("main"));
        c.name = "other";
        assert_eq!(root_reason(&c), Some("test"));
        let mut c = ctx("struct", "S", &none);
        c.vis = Some("pub");
        assert_eq!(root_reason(&c), Some("api"));
        c.vis = Some("pub(crate)");
        assert_eq!(root_reason(&c), None);
        let mut c = ctx("mod", "m", &none);
        c.vis = Some("pub");
        assert_eq!(root_reason(&c), None, "only code kinds");
    }
}
