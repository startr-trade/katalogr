//! The wire types of the timeline (FEAT-033): the points a time slider can show. No logic.

use crate::position::PositionRef;
use crate::view_answer::ViewQueryJson;
use schemars::JsonSchema;
use serde::Serialize;

/// Where a timeline point comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PointSource {
    StoredView,
    Snapshot,
    MetricSample,
}

/// One point a timeline can show: a stored view, or a view kept in a snapshot folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TimelinePoint {
    pub source: PointSource,
    pub view_id: Option<String>,
    pub query: Option<ViewQueryJson>,
    pub positions: Vec<PositionRef>,
    /// Unix seconds, the newest across `positions`. None when no commit could be read.
    pub committed: Option<i64>,
    /// The snapshot's folder name, never a full path. Only for `Snapshot`.
    pub snapshot: Option<String>,
}

/// The points in time order, oldest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Timeline {
    pub points: Vec<TimelinePoint>,
}
