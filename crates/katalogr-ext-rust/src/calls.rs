//! Call sites: path calls resolved through the bindings (extracted), method calls and macro
//! invocations matched by name (inferred) (FEAT-005 section 4, "Calls").

use crate::bindings::segments_of;
use crate::extract::{Builder, NS_MACRO, NS_METHOD};
use katalogr_model::{Confidence, ExportKey, Id};
use ra_ap_syntax::{AstNode, SyntaxNode, WalkEvent, ast};

/// Common standard-library method names. A method call with one of these names is not matched
/// by name: it would link to any workspace method of the same name (FEAT-053). Sorted.
const STD_METHODS: &[&str] = &[
    "as_ref",
    "as_str",
    "clone",
    "collect",
    "contains",
    "default",
    "eq",
    "filter",
    "fmt",
    "from",
    "get",
    "insert",
    "into",
    "into_iter",
    "is_empty",
    "iter",
    "iter_mut",
    "len",
    "map",
    "next",
    "ok",
    "push",
    "remove",
    "to_owned",
    "to_string",
    "unwrap",
    "with_capacity",
];

impl Builder<'_> {
    /// Every call below `node` becomes a reference from the symbol `from`. Calls inside macro
    /// arguments are token trees, not expressions, and are not seen.
    pub(crate) fn collect_calls(&mut self, node: &SyntaxNode, from: Id) {
        for ev in node.preorder() {
            let WalkEvent::Enter(n) = ev else { continue };
            if let Some(call) = ast::CallExpr::cast(n.clone()) {
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
