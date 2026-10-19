//! Package keys, contract construction and path helpers.

use crate::CONTRACT_KIND;
use katalogr_model::ContractName;

/// The package key both sides agree on: strip a leading `@scope/`, ASCII-lowercase, replace
/// `-` with `_`. `demo-wasm`, `@acme/demo-wasm`, `demo_wasm` all give `demo_wasm`.
pub fn normalise(name: &str) -> String {
    let base = match name.strip_prefix('@') {
        Some(rest) => rest.split_once('/').map_or(rest, |(_, n)| n),
        None => name,
    };
    base.to_ascii_lowercase().replace('-', "_")
}

/// `ContractName { kind: "wasm_export", key: "<package key>/<js path>" }`.
pub fn contract(package_key: &str, js_path: &str) -> ContractName {
    ContractName {
        kind: CONTRACT_KIND.to_string(),
        key: format!("{package_key}/{js_path}"),
    }
}

/// Names the generated glue adds that no Rust item provides: `default`, `initSync`, `free`,
/// and anything starting with `__`.
pub fn is_glue_only(js_name: &str) -> bool {
    matches!(js_name, "default" | "initSync" | "free")
        || js_name.starts_with("__wbg_")
        || js_name.starts_with("__wbindgen_")
        || js_name.starts_with("__")
}

/// Join a repo-relative dir and a relative path, normalising `.` and `..`; `None` when the
/// result leaves the repository or `rel` is absolute.
pub fn join_norm(dir: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') {
        return None;
    }
    let mut out: Vec<&str> = dir
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

/// The parent directory of a repo-relative path (`""` at the root).
pub fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

#[test]
fn normalise_strips_scope_and_dashes() {
    assert_eq!(normalise("@acme/Engine-Core"), "engine_core");
    assert_eq!(normalise("demo-wasm"), "demo_wasm");
    assert_eq!(normalise("demo_wasm"), "demo_wasm");
}

#[test]
fn ts_export_key_format_pinned() {
    // ext-ts's export_name format is `<file>#<name>`; the extern-import candidates rely on it.
    let file = join_norm("", "js/host.js").unwrap();
    assert_eq!(format!("{file}#{}", "now"), "js/host.js#now");
}

#[test]
fn joins_and_glue_names() {
    assert_eq!(join_norm("web", "../pkg").as_deref(), Some("pkg"));
    assert_eq!(join_norm("", "../x"), None);
    assert_eq!(join_norm("a", "/x"), None);
    assert_eq!(parent_dir("a/b/c.js"), "a/b");
    assert_eq!(parent_dir("c.js"), "");
    assert!(is_glue_only("default") && is_glue_only("__wbg_x") && is_glue_only("initSync"));
    assert!(!is_glue_only("greet"));
}
