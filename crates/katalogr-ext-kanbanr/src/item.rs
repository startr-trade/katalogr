//! Items and the charter: YAML files of the board.

use crate::{Build, find_line, str_fact};
use katalogr_model::annotate::{GOAL_NS, ITEM_NS, PREFIX_NS, REQ_NS};
use katalogr_model::{ExportKey, ExtractError, Fact, Segment, Span};
use serde_yaml_ng::Value;
use std::collections::BTreeMap;

fn parse(b: &Build<'_>) -> Result<Value, ExtractError> {
    serde_yaml_ng::from_str(b.text).map_err(|e| {
        let (line, col) = e
            .location()
            .map_or((1, 1), |l| (l.line() as u32, l.column() as u32));
        let (line, col) = (line.max(1), col.max(1));
        ExtractError::Parse {
            span: Some(Span {
                start_line: line,
                start_col: col,
                end_line: line,
                end_col: col,
            }),
            message: e.to_string(),
        }
    })
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect()
}

/// `projects/<p>/features/**/<CODE>.yaml`: the item, its requirements and tasks, and the goals
/// it serves.
pub(crate) fn extract(b: &mut Build<'_>) -> Result<(), ExtractError> {
    let doc = parse(b)?;
    let Some(code) = doc.get("code").and_then(Value::as_str).map(str::to_string) else {
        return Err(ExtractError::Parse {
            span: None,
            message: "item has no `code`".to_string(),
        });
    };
    let mut facts: BTreeMap<String, Fact> = BTreeMap::new();
    for key in ["title", "status", "kind", "milestone"] {
        if let Some(f) = str_fact(&doc, key) {
            facts.insert(key.to_string(), f);
        }
    }
    let whole = b.whole();
    b.facts = facts.clone();
    let (item, item_segs) = b.symbol(&[], "item", &code, whole, facts);
    b.export(ITEM_NS, code.clone(), item);
    b.export(
        PREFIX_NS,
        katalogr_model::annotate::prefix_of(&code).to_string(),
        item,
    );

    let definition = doc.get("definition");

    // Requirements.
    let def_line = find_line(b.text, 1, "definition:").unwrap_or(1);
    for r in definition
        .and_then(|d| d.get("requirements"))
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
    {
        let Some(id) = r.get("id").and_then(Value::as_str) else {
            continue;
        };
        let line = find_line(b.text, def_line, &format!("id: {id}"));
        let span = line.map_or(whole, |l| b.line_span(l));
        let mut f = BTreeMap::new();
        for key in ["text", "kind"] {
            if let Some(v) = str_fact(r, key) {
                f.insert(key.to_string(), v);
            }
        }
        let (rid, _) = b.symbol(&item_segs, "req", id, span, f);
        b.export(REQ_NS, format!("{code}/{id}"), rid);
    }

    // Tasks of the todo-lists: `task:TL-001/T1`.
    let todo_line = find_line(b.text, 1, "todo_lists:").unwrap_or(1);
    for list in doc
        .get("todo_lists")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
    {
        let Some(tl) = list.get("code").and_then(Value::as_str) else {
            continue;
        };
        let list_line = find_line(b.text, todo_line, &format!("code: {tl}")).unwrap_or(todo_line);
        for t in list
            .get("tasks")
            .and_then(Value::as_sequence)
            .into_iter()
            .flatten()
        {
            let Some(key) = t.get("key").and_then(Value::as_str) else {
                continue;
            };
            let line = find_line(b.text, list_line, &format!("key: {key}"));
            let span = line.map_or(whole, |l| b.line_span(l));
            let mut f = BTreeMap::new();
            for k in ["text", "state"] {
                if let Some(v) = str_fact(t, k) {
                    f.insert(k.to_string(), v);
                }
            }
            b.symbol(&item_segs, "task", &format!("{tl}/{key}"), span, f);
        }
    }

    // Goals the item serves.
    let goals_line = find_line(b.text, def_line, "goals:").unwrap_or(def_line);
    for g in strings(definition.and_then(|d| d.get("goals"))) {
        let line = find_line(b.text, goals_line + 1, &g);
        b.add_ref(
            item,
            "serves",
            line.map(|l| b.line_span(l)),
            vec![ExportKey {
                namespace: GOAL_NS.to_string(),
                key: g,
            }],
        );
    }
    Ok(())
}

/// `projects/<p>/charter.yaml`: the goals, as `goal:G-n`.
pub(crate) fn charter(b: &mut Build<'_>) -> Result<(), ExtractError> {
    let doc = parse(b)?;
    let goals_line = find_line(b.text, 1, "goals:").unwrap_or(1);
    for g in doc
        .get("goals")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
    {
        let Some(id) = g.get("id").and_then(Value::as_str) else {
            continue;
        };
        let span = find_line(b.text, goals_line, &format!("id: {id}"))
            .map_or_else(|| b.whole(), |l| b.line_span(l));
        let mut f = BTreeMap::new();
        for key in ["statement", "measure"] {
            if let Some(v) = str_fact(g, key) {
                f.insert(key.to_string(), v);
            }
        }
        let (gid, _) = b.symbol(&[] as &[Segment], "goal", id, span, f);
        b.export(GOAL_NS, id.to_string(), gid);
    }
    Ok(())
}
