//! Unit tests of `mcp::tools`; compiled as the crate-level module `tools` so they print as
//! `tools::<test>`.

use crate::exit::ExitCode;
use crate::mcp::tools::{Parsed, *};
use crate::render::json::JsonAnswer;
use rmcp::model::CallToolResult;
use serde_json::{Value, json};

fn ans(exit: ExitCode, stdout: Option<&str>, message: Option<&str>) -> JsonAnswer {
    JsonAnswer {
        exit,
        stdout: stdout.map(str::to_string),
        message: message.map(str::to_string),
        warnings: Vec::new(),
        diagnostic: Vec::new(),
    }
}

fn texts(r: &CallToolResult) -> Vec<String> {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect()
}

fn exit_of(r: &CallToolResult) -> i64 {
    r.meta.as_ref().unwrap().0["katalogr/exit"]
        .as_i64()
        .unwrap()
}

/// R-3. `structuredContent` and `outputSchema` need protocol 2025-06-18 or later.
#[test]
fn structured_only_from_2025_06_18() {
    assert!(has_structured(None));
    assert!(has_structured(Some("2025-06-18")));
    assert!(has_structured(Some("2026-07-28")));
    assert!(!has_structured(Some("2025-03-26")));
    assert!(!has_structured(Some("2024-11-05")));
    for t in list(true, false) {
        assert!(t.output_schema.is_none(), "{}", t.name);
    }
    let with = list(true, true);
    assert_eq!(
        with.iter().filter(|t| t.output_schema.is_some()).count(),
        10
    );
    for t in list(false, true) {
        assert!(t.output_schema.is_none(), "{}", t.name);
    }
}

/// R-4. The result mirrors the answer: content, `isError`, structured content and `_meta`.
#[test]
fn call_result_mirrors_answer() {
    let ok = to_call_result(&ans(ExitCode::Ok, Some("{\"schema\":1}"), None), true);
    assert_eq!(ok.is_error, Some(false));
    assert_eq!(texts(&ok), ["{\"schema\":1}"]);
    assert_eq!(ok.structured_content, Some(json!({"schema": 1})));
    assert_eq!(exit_of(&ok), 0);
    assert!(
        !ok.meta
            .as_ref()
            .unwrap()
            .0
            .contains_key("katalogr/warnings")
    );

    let amb = to_call_result(
        &ans(
            ExitCode::Ambiguous,
            Some("{\"schema\":1}"),
            Some("ambiguous: 2 candidates"),
        ),
        true,
    );
    assert_eq!(amb.is_error, Some(true));
    assert_eq!(texts(&amb), ["ambiguous: 2 candidates", "{\"schema\":1}"]);
    assert_eq!(amb.structured_content, None);
    assert_eq!(exit_of(&amb), 3);

    let none = to_call_result(
        &ans(ExitCode::NoMatch, None, Some("no symbol matches \"x\"")),
        true,
    );
    assert_eq!(none.is_error, Some(true));
    assert_eq!(texts(&none), ["no symbol matches \"x\""]);
    assert_eq!(none.structured_content, None);
    assert_eq!(exit_of(&none), 4);

    let dangling = to_call_result(&ans(ExitCode::Error, Some("{\"schema\":1}"), None), true);
    assert_eq!(dangling.is_error, Some(false));
    assert!(dangling.structured_content.is_some());
    assert_eq!(exit_of(&dangling), 1);

    let failed = to_call_result(&ans(ExitCode::Error, None, Some("boom")), true);
    assert_eq!(failed.is_error, Some(true));

    let stale = to_call_result(&ans(ExitCode::Stale, Some("{\"stale\":true}"), None), true);
    assert_eq!(stale.is_error, Some(false));
    assert_eq!(exit_of(&stale), 5);
    let stale_none = to_call_result(&ans(ExitCode::Stale, None, Some("stale")), true);
    assert_eq!(stale_none.is_error, Some(true));

    let mut warned = ans(ExitCode::Ok, Some("{}"), None);
    warned.warnings = vec!["answer may be stale (1 change(s) pending)".to_string()];
    let w = to_call_result(&warned, true);
    assert_eq!(
        w.meta.as_ref().unwrap().0["katalogr/warnings"],
        json!(["answer may be stale (1 change(s) pending)"])
    );

    let plain = to_call_result(&ans(ExitCode::Ok, Some("{\"schema\":1}"), None), false);
    assert_eq!(plain.structured_content, None);
    assert_eq!(texts(&plain), ["{\"schema\":1}"]);
}

#[test]
fn arguments_are_checked() {
    let spec = &TOOLS[2];
    assert_eq!(spec.name, "callers");
    let bad = |v: Value| Parsed::parse(spec, &v).err().unwrap();
    assert_eq!(bad(json!({})), "callers: symbol: required");
    assert_eq!(
        bad(json!({"symbol": 1})),
        "callers: symbol: expected string"
    );
    assert_eq!(
        bad(json!({"symbol": "a", "depth": 0})),
        "callers: depth: must be >= 1"
    );
    assert_eq!(
        bad(json!({"symbol": "a", "x": 0})),
        "callers: x: unknown argument"
    );
    assert!(Parsed::parse(spec, &json!({"symbol": "a", "budget": 5})).is_ok());
    assert_eq!(
        bad(json!({"symbol": "a", "expand": "yes"})),
        "callers: expand: expected boolean"
    );
    assert!(Parsed::parse(spec, &json!({"symbol": "a", "expand": true})).is_ok());
    let refs = &TOOLS[1];
    assert_eq!(refs.name, "refs");
    assert!(Parsed::parse(refs, &json!({"symbol": "a", "expand": false})).is_ok());
    assert_eq!(
        Parsed::parse(refs, &json!({"symbol": "a", "expand": 1}))
            .err()
            .unwrap(),
        "refs: expand: expected boolean"
    );
}
