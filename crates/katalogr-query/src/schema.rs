//! JSON Schemas of the `--json` output, one per command.

use crate::deps::DepHit;
use crate::doc_impact::DocImpact;
use crate::impact::ImpactReport;
use crate::position::PositionAnswer;
use crate::result::{
    CallerHit, DanglingRef, Envelope, Explain, MapEntry, MirrorDrift, PathStep, RefHit, SearchHit,
    ShowResult, SymbolHit, TestHit, WhyResult,
};
use crate::timeline::Timeline;
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
        ("deps", to_value(schema_for!(Envelope<Vec<DepHit>>))),
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
        ("docs-affected", to_value(schema_for!(Envelope<DocImpact>))),
        ("tests", to_value(schema_for!(Envelope<Vec<TestHit>>))),
        ("impact", to_value(schema_for!(Envelope<ImpactReport>))),
        ("timeline", to_value(schema_for!(Envelope<Timeline>))),
    ])
}

/// The schema of `katalogr position --json` (FEAT-084). It is not an envelope, so it is not
/// among [`schemas`].
pub fn position_schema() -> Value {
    schema_for!(PositionAnswer).to_value()
}
