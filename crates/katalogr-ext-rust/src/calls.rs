//! Call sites: path calls resolved through the bindings (extracted), method calls and macro
//! invocations matched by name (inferred) (FEAT-005 section 4, "Calls").

use crate::bindings::segments_of;
use crate::extract::{Builder, NS_MACRO, NS_METHOD};
use katalogr_model::{Confidence, ExportKey, Id};
use ra_ap_syntax::{AstNode, Edition, NodeOrToken, SyntaxKind, SyntaxNode, WalkEvent, ast};

/// Standard-library macros whose arguments are comma- or semicolon-separated expressions
/// (FEAT-120). Calls in their arguments are linked like calls outside a macro.
const STD_ARG_MACROS: &[&str] = &[
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "dbg",
    "eprint",
    "eprintln",
    "format",
    "format_args",
    "matches",
    "panic",
    "print",
    "println",
    "todo",
    "unimplemented",
    "unreachable",
    "vec",
    "write",
    "writeln",
];

/// How many macros may nest before their arguments are no longer read.
const MACRO_ARG_DEPTH: u32 = 4;

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
    /// Every call below `node` becomes a reference from the symbol `from`. The arguments of
    /// the macros in [`STD_ARG_MACROS`] are token trees: each comma- or semicolon-separated
    /// segment is re-parsed as an expression and walked too (FEAT-120). Other macros'
    /// arguments are not seen.
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
                if let Some(name) = segments_of(&path).and_then(|s| s.last().cloned()) {
                    self.macro_args(&mac, &name, from);
                }
            }
        }
    }

    /// Calls in the arguments of `mac` when it is an unshadowed standard macro (FEAT-120).
    fn macro_args(&mut self, mac: &ast::MacroCall, name: &str, from: Id) {
        if self.macro_depth >= MACRO_ARG_DEPTH || !STD_ARG_MACROS.contains(&name) {
            return;
        }
        let Some(tt) = mac.token_tree() else { return };
        if self.defines_macro(name) {
            return;
        }
        let mut elems: Vec<_> = tt.syntax().children_with_tokens().collect();
        if elems.len() < 2 {
            return;
        }
        elems.pop();
        elems.remove(0);
        let mut segments: Vec<Vec<_>> = vec![Vec::new()];
        for e in elems {
            let sep = matches!(&e, NodeOrToken::Token(t)
                if matches!(t.kind(), SyntaxKind::COMMA | SyntaxKind::SEMICOLON));
            if sep {
                segments.push(Vec::new());
            } else if let Some(last) = segments.last_mut() {
                last.push(e);
            }
        }
        for seg in segments {
            let is_trivia = |e: &ra_ap_syntax::SyntaxElement| {
                matches!(e.kind(), SyntaxKind::WHITESPACE | SyntaxKind::COMMENT)
            };
            let Some(first) = seg.iter().position(|e| !is_trivia(e)) else {
                continue;
            };
            let last = seg.iter().rposition(|e| !is_trivia(e)).unwrap_or(first);
            let text: String = seg[first..=last].iter().map(|e| e.to_string()).collect();
            let local: usize = seg[first].text_range().start().into();
            let parse = ast::Expr::parse(&text, Edition::Edition2021);
            let root = parse.syntax_node();
            if !parse.errors().is_empty() || ast::Expr::cast(root.clone()).is_none() {
                continue;
            }
            let saved = self.span_base;
            self.span_base = saved + local;
            self.macro_depth += 1;
            self.collect_calls(&root, from);
            self.macro_depth -= 1;
            self.span_base = saved;
        }
    }

    /// The file defines a `macro_rules!` called `name`, so a call of `name!` is not the
    /// standard macro. The names are collected once from the whole file in `extract`.
    fn defines_macro(&self, name: &str) -> bool {
        self.local_macros.contains(name)
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
