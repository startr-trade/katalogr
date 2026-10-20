//! Test files and test callbacks (FEAT-063 section 6.2, ADR-0018): in a test file each
//! `describe`/`suite`, `it`/`test` and setup or teardown call becomes a `suite`, `case` or
//! `hook` symbol with the neutral `test` fact, and the calls inside its callback belong to it.
//! Recognition is syntactic (ADR-0015): a local function named `it` counts as a test call.

use crate::items::{Builder, Pending};
use katalogr_model::insight::{
    FACT_TEST, TEST_CASE, TEST_NAME_NS, TEST_SUITE, TEST_SUPPORT, test_name_keys,
};
use katalogr_model::{Fact, Id, Segment};
use oxc_ast::ast::{
    ArrowFunctionBody, ArrowFunctionExpression, Expression, Function, Statement, TemplateLiteral,
};
use std::collections::BTreeMap;

/// Longest title kept, in characters.
const TITLE_MAX: usize = 120;

/// `*.test.*`, `*.spec.*` (on the file name) or a `__tests__` directory segment.
pub(crate) fn is_test_file(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    let file = parts.pop().unwrap_or("");
    file.contains(".test.") || file.contains(".spec.") || parts.contains(&"__tests__")
}

/// The file name without its extension and without a trailing `.test` / `.spec`:
/// `math.test.ts` gives `math`.
pub(crate) fn file_stem(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or("");
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    let stem = stem
        .strip_suffix(".test")
        .or_else(|| stem.strip_suffix(".spec"))
        .unwrap_or(stem);
    stem.to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestCallKind {
    Suite,
    Case,
    Hook,
}

pub(crate) enum Callback<'p, 'a> {
    Arrow(&'p ArrowFunctionExpression<'a>),
    Func(&'p Function<'a>),
}

pub(crate) struct TestCall<'p, 'a> {
    pub kind: TestCallKind,
    /// The test title, or the hook's callee name.
    pub title: String,
    pub callback: Callback<'p, 'a>,
    pub span: oxc_span::Span,
}

const MODIFIERS: [&str; 5] = ["only", "skip", "concurrent", "failing", "todo"];

/// The base name of a test callee: `it`, `it.only`, `it.each(table)`.
fn base_name<'p>(callee: &'p Expression<'_>) -> Option<&'p str> {
    match callee.get_inner_expression() {
        Expression::Identifier(n) => Some(n.name.as_str()),
        Expression::StaticMemberExpression(m) => {
            let Expression::Identifier(n) = m.object.get_inner_expression() else {
                return None;
            };
            MODIFIERS
                .contains(&m.property.name.as_str())
                .then_some(n.name.as_str())
        }
        Expression::CallExpression(c) => {
            let Expression::StaticMemberExpression(m) = c.callee.get_inner_expression() else {
                return None;
            };
            let Expression::Identifier(n) = m.object.get_inner_expression() else {
                return None;
            };
            (m.property.name.as_str() == "each").then_some(n.name.as_str())
        }
        _ => None,
    }
}

fn template_text(t: &TemplateLiteral<'_>) -> Option<String> {
    if !t.expressions.is_empty() {
        return None;
    }
    let q = t.quasis.first()?;
    Some(
        q.value
            .cooked
            .as_ref()
            .map_or_else(|| q.value.raw.to_string(), |c| c.to_string()),
    )
}

fn truncate(s: String) -> String {
    if s.chars().count() > TITLE_MAX {
        s.chars().take(TITLE_MAX).collect()
    } else {
        s
    }
}

/// Recognises a test call in an expression statement; None otherwise.
pub(crate) fn test_call<'p, 'a>(
    stmt: &'p Statement<'a>,
    b: &Builder<'_>,
) -> Option<TestCall<'p, 'a>> {
    let Statement::ExpressionStatement(es) = stmt else {
        return None;
    };
    let Expression::CallExpression(c) = es.expression.get_inner_expression() else {
        return None;
    };
    let base = base_name(&c.callee)?;
    let kind = match base {
        "describe" | "suite" => TestCallKind::Suite,
        "it" | "test" => TestCallKind::Case,
        "beforeEach" | "afterEach" | "beforeAll" | "afterAll" | "before" | "after" => {
            TestCallKind::Hook
        }
        _ => return None,
    };
    let callback =
        c.arguments
            .iter()
            .rev()
            .find_map(|a| match a.as_expression()?.get_inner_expression() {
                Expression::ArrowFunctionExpression(f) => Some(Callback::Arrow(f)),
                Expression::FunctionExpression(f) => Some(Callback::Func(f)),
                _ => None,
            })?;
    let title = if kind == TestCallKind::Hook {
        base.to_string()
    } else {
        match c.arguments.first().and_then(|a| a.as_expression()) {
            Some(e) => match e.get_inner_expression() {
                Expression::StringLiteral(s) => s.value.to_string(),
                Expression::TemplateLiteral(t) => {
                    template_text(t).unwrap_or_else(|| b.source_text(oxc_span::GetSpan::span(e)))
                }
                other => b.source_text(oxc_span::GetSpan::span(other)),
            },
            None => String::new(),
        }
    };
    Some(TestCall {
        kind,
        title: truncate(title),
        callback,
        span: c.span,
    })
}

impl Builder<'_> {
    /// One-line, whitespace-normalised source text of a span.
    pub(crate) fn source_text(&self, s: oxc_span::Span) -> String {
        let raw = self
            .text
            .get(s.start as usize..s.end as usize)
            .unwrap_or("");
        raw.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Declares `stmt` as a test symbol below `parent` (None at top level): the parent's id,
    /// its segments and its title path. Returns false (and does nothing) when `stmt` is not a
    /// test call.
    pub(crate) fn declare_test<'p, 'a>(
        &mut self,
        stmt: &'p Statement<'a>,
        parent: Option<(Id, &[Segment], &[String])>,
        pending: &mut Vec<Pending<'p, 'a>>,
    ) -> bool {
        let Some(call) = test_call(stmt, self) else {
            return false;
        };
        let (kind, role) = match call.kind {
            TestCallKind::Suite => ("suite", TEST_SUITE),
            TestCallKind::Case => ("case", TEST_CASE),
            TestCallKind::Hook => ("hook", TEST_SUPPORT),
        };
        let mut segments: Vec<Segment> = parent.map_or_else(Vec::new, |p| p.1.to_vec());
        segments.push(Self::seg(kind, &call.title));
        let mut titles: Vec<String> = parent.map_or_else(Vec::new, |p| p.2.to_vec());
        let is_async = match &call.callback {
            Callback::Arrow(a) => a.r#async,
            Callback::Func(f) => f.r#async,
        };
        let mut facts: BTreeMap<String, Fact> = BTreeMap::new();
        facts.insert(FACT_TEST.to_string(), Fact::Str(role.to_string()));
        if is_async {
            facts.insert("async".to_string(), Fact::Bool(true));
        }
        let span = self.span(call.span);
        let id = self.add_symbol(segments.clone(), kind, span, facts);
        if let Some((pid, _, _)) = parent {
            self.contains(pid, id, span);
        }
        if call.kind == TestCallKind::Case {
            let stem = file_stem(self.path.as_str());
            let mut path: Vec<&str> = vec![stem.as_str()];
            path.extend(titles.iter().map(String::as_str));
            path.push(call.title.as_str());
            for k in test_name_keys(&path) {
                self.export(TEST_NAME_NS, k, id);
            }
        }
        match (call.kind, call.callback) {
            (TestCallKind::Suite, Callback::Arrow(a))
                if matches!(a.body, ArrowFunctionBody::FunctionBody(_)) =>
            {
                let ArrowFunctionBody::FunctionBody(body) = &a.body else {
                    unreachable!("guarded by the match arm");
                };
                titles.push(call.title.clone());
                self.declare_suite_body(id, &segments, &titles, &body.statements, pending);
            }
            (TestCallKind::Suite, Callback::Func(f)) => {
                titles.push(call.title.clone());
                if let Some(body) = &f.body {
                    self.declare_suite_body(id, &segments, &titles, &body.statements, pending);
                }
            }
            (_, Callback::Arrow(a)) => pending.push(Pending::Arrow(id, a)),
            (_, Callback::Func(f)) => pending.push(Pending::Func(id, f)),
        }
        true
    }

    fn declare_suite_body<'p, 'a>(
        &mut self,
        id: Id,
        segments: &[Segment],
        titles: &[String],
        statements: &'p [Statement<'a>],
        pending: &mut Vec<Pending<'p, 'a>>,
    ) {
        for s in statements {
            if !self.declare_test(s, Some((id, segments, titles)), pending) {
                pending.push(Pending::Within(id, s));
            }
        }
    }

    /// In a test file: `test = support` on every symbol with no `test` fact.
    pub(crate) fn mark_support(&mut self) {
        for s in &mut self.symbols {
            s.facts
                .entry(FACT_TEST.to_string())
                .or_insert_with(|| Fact::Str(TEST_SUPPORT.to_string()));
        }
    }
}

#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use super::*;

    #[test]
    fn test_file_names() {
        assert!(is_test_file("src/a.test.ts"));
        assert!(is_test_file("a.spec.js"));
        assert!(is_test_file("src/__tests__/util.ts"));
        assert!(!is_test_file("src/runner.ts"));
        assert!(!is_test_file("src/contest.ts"));
        assert!(!is_test_file("test/a.ts"));
        assert!(!is_test_file("src/tests.ts"));
    }

    #[test]
    fn file_stems() {
        assert_eq!(file_stem("src/math.test.ts"), "math");
        assert_eq!(file_stem("a/b.spec.js"), "b");
        assert_eq!(file_stem("src/__tests__/util.ts"), "util");
        assert_eq!(file_stem("plain.ts"), "plain");
        assert_eq!(file_stem("noext"), "noext");
    }
}
