//! Call sites: path calls resolved through the bindings (extracted), method calls and macro
//! invocations matched by name (inferred) (FEAT-005 section 4, "Calls").

use crate::bindings::segments_of;
use crate::extract::{Builder, NS_MACRO, NS_METHOD};
use katalogr_model::{Confidence, ExportKey, Id};
use ra_ap_syntax::{AstNode, SyntaxNode, WalkEvent, ast};

/// Common standard-library method names. A method call with one of these names yields no
/// candidate: it would link to any workspace method of the same name (FEAT-053, FEAT-058).
/// Sorted, unique, lower snake case.
const STD_METHODS: &[&str] = &[
    "and_then",
    "any",
    "append",
    "as_bytes",
    "as_ref",
    "as_str",
    "chars",
    "clone",
    "cloned",
    "cmp",
    "collect",
    "contains",
    "count",
    "default",
    "display",
    "ends_with",
    "enumerate",
    "eq",
    "expect",
    "extend",
    "extend_from_slice",
    "filter",
    "find",
    "fmt",
    "from",
    "get",
    "insert",
    "into",
    "into_iter",
    "is_empty",
    "is_some_and",
    "iter",
    "iter_mut",
    "join",
    "last",
    "len",
    "lines",
    "map",
    "map_err",
    "map_or",
    "next",
    "ok",
    "ok_or_else",
    "parse",
    "path",
    "position",
    "push",
    "push_str",
    "read",
    "remove",
    "replace",
    "sort",
    "split",
    "starts_with",
    "strip_prefix",
    "to_owned",
    "to_str",
    "to_string",
    "to_vec",
    "trim",
    "unwrap",
    "unwrap_or",
    "unwrap_or_default",
    "unwrap_or_else",
    "with_capacity",
    "write",
];

impl Builder<'_> {
    /// Every call below `node` becomes a reference from the symbol `from`. Calls inside macro
    /// arguments are token trees, not expressions, and are not seen.
    pub(crate) fn collect_calls(&mut self, node: &SyntaxNode, from: Id) {
        for ev in node.preorder() {
            let WalkEvent::Enter(n) = ev else { continue };
            if let Some(call) = ast::CallExpr::cast(n.clone()) {
                if let Some(sc) = self.str_call_of_call(&call) {
                    self.str_calls.entry(from).or_default().push(sc);
                }
                let Some(ast::Expr::PathExpr(pe)) = call.expr() else {
                    continue;
                };
                let Some(path) = pe.path() else { continue };
                let Some(segs) = segments_of(&path) else {
                    continue;
                };
                let cands = self.candidates(&segs, false);
                let span = self.span_of(path.syntax());
                self.add_ref(from, "calls", span, cands, Confidence::Extracted);
            } else if let Some(call) = ast::MethodCallExpr::cast(n.clone()) {
                if let Some(sc) = self.str_call_of_method(&call) {
                    self.str_calls.entry(from).or_default().push(sc);
                }
                let Some(name) = call.name_ref() else {
                    continue;
                };
                let text = name.text().to_string();
                if STD_METHODS.contains(&text.as_str()) {
                    continue;
                }
                let cands = vec![ExportKey {
                    namespace: NS_METHOD.to_string(),
                    key: name.text().to_string(),
                }];
                let span = self.span_of(name.syntax());
                self.add_ref(from, "calls", span, cands, Confidence::Inferred);
            } else if let Some(mac) = ast::MacroCall::cast(n) {
                let Some(path) = mac.path() else { continue };
                let Some(name) = segments_of(&path).and_then(|s| s.last().cloned()) else {
                    continue;
                };
                let cands = vec![ExportKey {
                    namespace: NS_MACRO.to_string(),
                    key: name,
                }];
                let span = self.span_of(path.syntax());
                self.add_ref(from, "calls", span, cands, Confidence::Inferred);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::STD_METHODS;

    #[test]
    fn std_methods_sorted_unique() {
        assert!(STD_METHODS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(STD_METHODS.len(), 66);
        for n in [
            "path",
            "find",
            "read",
            "write",
            "cmp",
            "append",
            "replace",
            "position",
            "parse",
            "join",
            "map_err",
            "and_then",
            "unwrap_or",
        ] {
            assert!(STD_METHODS.contains(&n), "{n}");
        }
    }
}
