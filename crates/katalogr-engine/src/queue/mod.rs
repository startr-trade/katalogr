//! The durable change queue: inbox spool, processor (FEAT-047, ADR-0008, ADR-0014).

pub mod inbox;
pub(crate) mod lock;
mod plan;
pub mod processor;

pub use inbox::Inbox;
pub use processor::{Processor, find_orphan_temps};

use katalogr_model::RelPath;
use serde::{Deserialize, Serialize};

/// A sequence number, assigned per context at ingest. 0 means none.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Seq(pub u64);

/// What an event applies to. M1 has one context per repository: `worktree`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Context(pub String);

impl Context {
    pub const WORKTREE: &'static str = "worktree";

    pub fn worktree() -> Context {
        Context(Self::WORKTREE.to_string())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    Changed,
    Deleted,
    Rescan,
}

/// An ingested event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeEvent {
    /// File stem of the inbox entry.
    pub id: String,
    pub context: Context,
    pub repo: String,
    /// `None` for a rescan of the whole repository.
    pub path: Option<RelPath>,
    pub kind: EventKind,
}

/// What a producer hands to [`Inbox::append`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewEvent {
    pub context: Context,
    pub repo: String,
    pub path: Option<RelPath>,
    pub kind: EventKind,
}

impl NewEvent {
    /// A change or deletion of one file in the `worktree` context.
    pub fn file(repo: &str, path: &RelPath, kind: EventKind) -> NewEvent {
        NewEvent {
            context: Context::worktree(),
            repo: repo.to_string(),
            path: Some(path.clone()),
            kind,
        }
    }

    /// A rescan of a whole repository in the `worktree` context.
    pub fn rescan(repo: &str) -> NewEvent {
        NewEvent {
            context: Context::worktree(),
            repo: repo.to_string(),
            path: None,
            kind: EventKind::Rescan,
        }
    }
}
