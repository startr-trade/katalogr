//! Leading symbols of the insight reports, for the next-query hints.

use crate::result::{CommunityReport, CycleReport, DeadCodeReport, HubReport, RationaleReport};
use katalogr_query::HintSource;

impl HintSource for HubReport {
    fn lead(&self) -> Option<String> {
        self.items.first().map(|h| h.node.name.clone())
    }
}

impl HintSource for CommunityReport {
    fn lead(&self) -> Option<String> {
        self.items.first().map(|c| c.label.name.clone())
    }
}

/// A cycle has no single symbol to follow up on.
impl HintSource for CycleReport {
    fn lead(&self) -> Option<String> {
        None
    }
}

/// Candidates are to be checked one by one; no follow-up is suggested.
impl HintSource for DeadCodeReport {
    fn lead(&self) -> Option<String> {
        None
    }
}

impl HintSource for RationaleReport {
    fn lead(&self) -> Option<String> {
        self.items.first().map(|e| e.node.name.clone())
    }
}
