//! References found in parsed statements (§4): relations minus CTE names, routine calls and
//! routine bodies.

use crate::dialect::{Dialect, chain};
use crate::names::{is_system_relation, normalise_ident, routine_key, table_key};
use crate::scan::{Refs, scan_body};
use sqlparser::ast::{
    CreateFunction, CreateFunctionBody, Expr, ObjectName, ObjectNamePart, Query, Statement, Value,
    Visit, Visitor,
};
use sqlparser::parser::Parser;
use std::collections::BTreeSet;
use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The name parts of an object name as quoted text, so that the key functions normalise them.
pub(crate) fn name_parts(name: &ObjectName) -> Vec<String> {
    name.0
        .iter()
        .filter_map(|p| match p {
            ObjectNamePart::Identifier(i) => Some(format!("\"{}\"", i.value.replace('"', "\"\""))),
            _ => None,
        })
        .collect()
}

pub(crate) fn as_strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// Collects relations, CTE names and qualified function calls while visiting.
#[derive(Default)]
pub(crate) struct Collector {
    relations: Vec<ObjectName>,
    ctes: BTreeSet<String>,
    functions: Vec<ObjectName>,
}

impl Visitor for Collector {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.ctes.insert(normalise_ident(&cte.alias.name.value));
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<()> {
        self.relations.push(relation.clone());
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        if let Expr::Function(f) = expr
            && f.name.0.len() >= 2
        {
            self.functions.push(f.name.clone());
        }
        ControlFlow::Continue(())
    }
}

impl Collector {
    pub(crate) fn default_() -> Collector {
        Collector::default()
    }

    /// Visit `v` and collect.
    pub(crate) fn of<V: Visit>(v: &V) -> Collector {
        let mut c = Collector::default();
        let _ = v.visit(&mut c);
        c
    }

    /// Visit more nodes into the same collector.
    pub(crate) fn add<V: Visit>(&mut self, v: &V) {
        let _ = v.visit(self);
    }

    /// Keys of the relations (CTE names and system catalogs excluded) and of the qualified
    /// function calls.
    pub(crate) fn refs(&self, schema: &str) -> Refs {
        let mut r = Refs::default();
        for rel in &self.relations {
            let parts = name_parts(rel);
            if parts.is_empty() {
                continue;
            }
            let p = as_strs(&parts);
            if is_system_relation(&p) {
                continue;
            }
            if p.len() == 1 && self.ctes.contains(&normalise_ident(p[0])) {
                continue;
            }
            r.tables.insert(table_key(&p, schema));
        }
        for f in &self.functions {
            let parts = name_parts(f);
            let p = as_strs(&parts);
            if is_system_relation(&p) {
                continue;
            }
            r.routines.insert(routine_key(&p, schema));
        }
        r
    }
}

/// Refs of a parsed statement: relations minus CTE names, and qualified function calls.
pub fn relations(stmt: &Statement, default_schema: &str) -> Refs {
    Collector::of(stmt).refs(default_schema)
}

/// How a routine's body was read.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BodyKind {
    Parsed,
    Scanned,
    None,
}

impl BodyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BodyKind::Parsed => "parsed",
            BodyKind::Scanned => "scanned",
            BodyKind::None => "none",
        }
    }
}

/// Parse `text` with the chain of `dialect`; the first success wins. Panics are caught.
pub(crate) fn parse_chain(
    text: &str,
    known: Option<Dialect>,
) -> Result<(Vec<Statement>, Dialect), String> {
    let mut first_err: Option<String> = None;
    for &d in chain(known) {
        match parse_one(text, d) {
            Ok(s) => return Ok((s, d)),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    Err(first_err.unwrap_or_else(|| "empty".to_string()))
}

pub(crate) fn parse_one(text: &str, d: Dialect) -> Result<Vec<Statement>, String> {
    let r = catch_unwind(AssertUnwindSafe(|| {
        Parser::new(d.parser())
            .with_recursion_limit(50)
            .try_with_sql(text)
            .and_then(|mut p| p.parse_statements())
    }));
    match r {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("parser panicked".to_string()),
    }
}

fn string_of(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Value(v) => match &v.value {
            Value::SingleQuotedString(s) | Value::EscapedStringLiteral(s) => Some(s.clone()),
            Value::DollarQuotedString(d) => Some(d.value.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// References of a function body and how it was read (§4): `language` is `sql`, or absent and
/// the body parses: parsed; a begin/end body is already statements; any other language, or a
/// string body that does not parse, is scanned; no body gives `None`.
pub fn routine_body(func: &CreateFunction, dialect: Dialect, schema: &str) -> (Refs, BodyKind) {
    let lang = func.language.as_ref().map(|l| l.value.to_lowercase());
    let sql_lang = lang.as_deref().is_none_or(|l| l == "sql");
    let Some(body) = &func.function_body else {
        return (Refs::default(), BodyKind::None);
    };
    match body {
        CreateFunctionBody::AsBeginEnd(b) => {
            (Collector::of(&b.statements).refs(schema), BodyKind::Parsed)
        }
        CreateFunctionBody::Return(e) | CreateFunctionBody::AsReturnExpr(e) => {
            (Collector::of(e).refs(schema), BodyKind::Parsed)
        }
        CreateFunctionBody::AsReturnSelect(s) => (Collector::of(s).refs(schema), BodyKind::Parsed),
        CreateFunctionBody::AsBeforeOptions { body, .. }
        | CreateFunctionBody::AsAfterOptions(body) => {
            let Some(text) = string_of(body) else {
                return (Collector::of(body).refs(schema), BodyKind::Parsed);
            };
            if sql_lang && let Ok((stmts, _)) = parse_chain(&text, Some(dialect)) {
                return (Collector::of(&stmts).refs(schema), BodyKind::Parsed);
            }
            (scan_body(&text, schema), BodyKind::Scanned)
        }
    }
}
