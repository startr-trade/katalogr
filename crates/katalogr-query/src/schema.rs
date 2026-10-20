//! JSON Schemas of the `--json` output, one per command.

use crate::result::{
    CallerHit, DanglingRef, Envelope, Explain, MapEntry, MirrorDrift, PathStep, RefHit, SearchHit,
    ShowResult, SymbolHit, TestHit, WhyResult,
};
use schemars::schema_for;
use serde_json::Value;
use std::collections::BTreeMap;

/// Command name to the schema of its envelope. Written to `schema/v1/<command>.schema.json`.
pub fn schemas() -> BTreeMap<&'static str, Value> {
    let to_value = |s: schemars::Schema| s.to_value();
    BTreeMap::from([
        ("find", to_value(schema_for!(Envelope<Vec<SymbolHit>>))),
        ("refs", to_value(schema_for!(Envelope<Vec<RefHit>>))),
        ("callers", to_value(schema_for!(Envelope<Vec<CallerHit>>))),
        ("why", to_value(schema_for!(Envelope<WhyResult>))),
        ("map", to_value(schema_for!(Envelope<Vec<MapEntry>>))),
        (
            "dangling",
            to_value(schema_for!(Envelope<Vec<DanglingRef>>)),
        ),
        ("mirrors", to_value(schema_for!(Envelope<Vec<MirrorDrift>>))),
        ("show", to_value(schema_for!(Envelope<ShowResult>))),
        ("search", to_value(schema_for!(Envelope<Vec<SearchHit>>))),
        ("path", to_value(schema_for!(Envelope<Vec<PathStep>>))),
        ("explain", to_value(schema_for!(Envelope<Explain>))),
        ("tests", to_value(schema_for!(Envelope<Vec<TestHit>>))),
    ])
}
