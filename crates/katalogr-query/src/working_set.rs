//! The working set (FEAT-039): the paths a person is likely working on, and the boost that
//! favours them in ranking. Pure and neutral: this module names no language, format or protocol,
//! and never reads git, the clock or the graph. The caller gathers the inputs.

use katalogr_model::{ArtifactName, Id, Name, RelPath};
use schemars::JsonSchema;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Why a path is in the working set. A path takes its strongest tier only (`Dirty` is the
/// strongest, so the derived order puts it first).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
pub enum Tier {
    Dirty,
    Recent,
    Branch,
    Item,
}

impl Tier {
    fn name(self) -> &'static str {
        match self {
            Tier::Dirty => "dirty",
            Tier::Recent => "recent",
            Tier::Branch => "branch",
            Tier::Item => "item",
        }
    }
}

/// The working set: `(repo, path)` to the strongest tier that holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkingSet {
    pub paths: BTreeMap<(String, RelPath), Tier>,
}

/// What the answer says about the working set, so a reader can see why the order differs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorkingSetSummary {
    pub dirty: usize,
    pub recent: usize,
    pub branch: usize,
    pub item_paths: usize,
    /// The active item, when there is one. Filled by the caller; [`WorkingSet::counts`] leaves
    /// it empty.
    pub item: Option<String>,
}

/// Boost weight per tier. Provisional defaults; FEAT-039 T7 replaces them from a sweep.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Weights {
    pub dirty: f64,
    pub recent: f64,
    pub branch: f64,
    pub item: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Weights {
            dirty: 0.5,
            recent: 0.4,
            branch: 0.3,
            item: 0.25,
        }
    }
}

impl Weights {
    fn of(&self, t: Tier) -> f64 {
        match t {
            Tier::Dirty => self.dirty,
            Tier::Recent => self.recent,
            Tier::Branch => self.branch,
            Tier::Item => self.item,
        }
    }
}

impl WorkingSet {
    /// Put a path in the set at `tier`, keeping the stronger tier when it is already there.
    pub fn add(&mut self, repo: &str, path: RelPath, tier: Tier) {
        self.paths
            .entry((repo.to_string(), path))
            .and_modify(|t| *t = (*t).min(tier))
            .or_insert(tier);
    }

    /// The tier of a path, if it is in the set.
    pub fn tier(&self, repo: &str, path: &RelPath) -> Option<Tier> {
        self.paths.get(&(repo.to_string(), path.clone())).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// SHA-256 (lower hex) of the sorted entries; goes into the view ID's arguments. The same
    /// entries give the same hash however they were added.
    pub fn hash(&self) -> String {
        let mut h = Sha256::new();
        for ((repo, path), tier) in &self.paths {
            h.update(repo.as_bytes());
            h.update([0]);
            h.update(path.as_str().as_bytes());
            h.update([0]);
            h.update(tier.name().as_bytes());
            h.update(b"\n");
        }
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Paths per tier.
    pub fn counts(&self) -> WorkingSetSummary {
        let mut s = WorkingSetSummary::default();
        for t in self.paths.values() {
            match t {
                Tier::Dirty => s.dirty += 1,
                Tier::Recent => s.recent += 1,
                Tier::Branch => s.branch += 1,
                Tier::Item => s.item_paths += 1,
            }
        }
        s
    }
}

fn boosted(value: f64, tier: Tier, w: &Weights) -> f64 {
    let v = value * (1.0 + w.of(tier));
    (v * 1e6).round() / 1e6
}

/// `value * (1 + weight of the path's tier)`, rounded to 6 decimals. A path outside the set
/// (so any path, for an empty set) keeps `value` exactly, byte for byte. Ties keep the caller's
/// existing tie-break, so the order stays total.
pub fn boost(value: f64, repo: &str, path: &RelPath, ws: &WorkingSet, w: &Weights) -> f64 {
    match ws.tier(repo, path) {
        None => value,
        Some(t) => boosted(value, t, w),
    }
}

/// A working set prepared for ranking: tiers by artifact id, so a ranking that knows a symbol's
/// artifact needs no path lookup.
#[derive(Clone, Debug)]
pub struct Boost {
    tiers: BTreeMap<Id, Tier>,
    weights: Weights,
}

impl Boost {
    /// `None` for an empty set: ranking then runs exactly as it does without one.
    pub fn new(ws: &WorkingSet, weights: Weights) -> Option<Boost> {
        if ws.is_empty() {
            return None;
        }
        let tiers = ws
            .paths
            .iter()
            .map(|((repo, path), t)| {
                (
                    ArtifactName {
                        repo: repo.clone(),
                        path: path.clone(),
                    }
                    .id(),
                    *t,
                )
            })
            .collect();
        Some(Boost { tiers, weights })
    }

    /// [`boost`] for a value belonging to the artifact `artifact`.
    pub fn apply(&self, value: f64, artifact: Id) -> f64 {
        match self.tiers.get(&artifact) {
            None => value,
            Some(t) => boosted(value, *t, &self.weights),
        }
    }
}
