//! JSON Schemas of the insight envelopes, one per command.

use crate::coverage::CoverageReport;
use crate::result::{CommunityReport, CycleReport, DeadCodeReport, HubReport, RationaleReport};
use katalogr_query::result::Envelope;
use schemars::schema_for;
use serde_json::Value;
use std::collections::BTreeMap;

/// Command name to the schema of its envelope: `hubs`, `communities`, `cycles`, `dead-code`,
/// `rationale` and `coverage`. Written to `schema/v1/<command>.schema.json`.
pub fn schemas() -> BTreeMap<&'static str, Value> {
    let to_value = |s: schemars::Schema| s.to_value();
    BTreeMap::from([
        ("hubs", to_value(schema_for!(Envelope<HubReport>))),
        (
            "communities",
            to_value(schema_for!(Envelope<CommunityReport>)),
        ),
        ("coverage", to_value(schema_for!(Envelope<CoverageReport>))),
        ("cycles", to_value(schema_for!(Envelope<CycleReport>))),
        ("dead-code", to_value(schema_for!(Envelope<DeadCodeReport>))),
        (
            "rationale",
            to_value(schema_for!(Envelope<RationaleReport>)),
        ),
    ])
}
