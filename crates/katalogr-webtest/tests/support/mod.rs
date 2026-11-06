//! Canned export data built from the harness fixtures (all names synthetic).
#![allow(dead_code)]

use katalogr_web::export::{
    EmbeddedAnswer, ExportData, PlanOptions, PlannedCall, PositionRef, answer_key, plan_symbol,
    plan_views,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub const SYMBOL: &str = "repo:app#fn:target";

pub fn fx(name: &str) -> Value {
    let text = match name {
        "meta" => include_str!("../../fixtures/meta.json"),
        "views-catalogue" => include_str!("../../fixtures/views-catalogue.json"),
        "view-structure" => include_str!("../../fixtures/view-structure.json"),
        "view-callers" => include_str!("../../fixtures/view-callers.json"),
        "view-contracts" => include_str!("../../fixtures/view-contracts.json"),
        "explain" => include_str!("../../fixtures/explain.json"),
        "why" => include_str!("../../fixtures/why.json"),
        "tests" => include_str!("../../fixtures/tests.json"),
        "show" => include_str!("../../fixtures/show.json"),
        "search" => include_str!("../../fixtures/search.json"),
        "hubs" => include_str!("../../fixtures/hubs.json"),
        "cycles" => include_str!("../../fixtures/cycles.json"),
        "communities" => include_str!("../../fixtures/communities.json"),
        "dead-code" => include_str!("../../fixtures/dead-code.json"),
        "coverage" => include_str!("../../fixtures/coverage.json"),
        other => panic!("no fixture {other}"),
    };
    serde_json::from_str(text).expect("fixture parses")
}

pub fn position() -> PositionRef {
    PositionRef {
        repo: "app".into(),
        commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        dirty: None,
    }
}

pub fn put(data: &mut ExportData, op: &str, params: &[(&str, &str)], body: Value) {
    let call = PlannedCall {
        op: op.into(),
        params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    };
    data.answers
        .insert(answer_key(&call), EmbeddedAnswer { status: 200, body });
}

/// An export with no answers.
pub fn empty() -> ExportData {
    ExportData {
        format: 1,
        katalogr: "0.1.0".into(),
        with_source: false,
        capabilities: fx("meta")["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_string())
            .collect(),
        positions: vec![position()],
        repos: vec!["app".into()],
        symbols: Vec::new(),
        partial: Vec::new(),
        answers: BTreeMap::new(),
    }
}

/// An export holding the views and one symbol's page.
pub fn canned(with_source: bool) -> ExportData {
    let mut d = empty();
    d.with_source = with_source;
    d.symbols = vec![fx("hubs")["result"]["items"][0]["node"].clone()];
    put(&mut d, "views/", &[], fx("views-catalogue"));
    put(&mut d, "views/structure", &[], fx("view-structure"));
    put(&mut d, "views/contracts", &[], fx("view-contracts"));
    put(&mut d, "hubs", &[("limit", "20")], fx("hubs"));
    put(&mut d, "cycles", &[("limit", "20")], fx("cycles"));
    put(&mut d, "communities", &[("limit", "20")], fx("communities"));
    put(&mut d, "dead-code", &[("limit", "20")], fx("dead-code"));
    put(&mut d, "coverage", &[], fx("coverage"));
    put(&mut d, "explain", &[("symbol", SYMBOL)], fx("explain"));
    put(&mut d, "why", &[("location", "src/lib.rs:3")], fx("why"));
    put(&mut d, "tests", &[("text", SYMBOL)], fx("tests"));
    put(
        &mut d,
        "views/callers",
        &[("symbol", SYMBOL)],
        fx("view-callers"),
    );
    let mut show = vec![("symbol", SYMBOL)];
    if !with_source {
        show.push(("signature", "true"));
    }
    put(&mut d, "show", &show, fx("show"));
    d
}

/// Every planned key for the canned symbol, for tests of the plan.
pub fn planned_keys(d: &ExportData) -> Vec<String> {
    let mut calls = plan_views(&d.capabilities);
    calls.extend(plan_symbol(
        SYMBOL,
        "src/lib.rs",
        3,
        &PlanOptions {
            with_source: d.with_source,
        },
    ));
    calls.iter().map(answer_key).collect()
}
