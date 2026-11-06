//! The timeline (FEAT-033): every view the project has kept, oldest first, from the view store
//! and from snapshot folders. It reads no index, runs no query and waits for no queue.

use crate::view_store::{parse_head, valid_id};
use crate::{DistError, Katalogr, Outcome, Reply, Report, ViewBytes, doc_views::SnapshotIndex};
use katalogr_query::{Partial, PointSource, PositionRef, Timeline, TimelinePoint, cap};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The point count `timeline` returns when the caller gives none.
pub const DEFAULT_TIMELINE_LIMIT: usize = 1000;

/// One snapshot folder: its own name and where it is.
struct SnapshotDir {
    name: String,
    path: PathBuf,
}

fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir())
}

fn is_real_file(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file())
}

fn dir_name(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("snapshot")
        .to_string()
}

/// The folder itself when it holds `index.json`, else each immediate child that does.
/// Symlinks are not followed.
fn snapshot_dirs(given: &[PathBuf]) -> Vec<SnapshotDir> {
    let mut out = Vec::new();
    for dir in given {
        if !is_real_dir(dir) {
            continue;
        }
        if is_real_file(&dir.join("index.json")) {
            out.push(SnapshotDir {
                name: dir_name(dir),
                path: dir.clone(),
            });
            continue;
        }
        let Ok(children) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut kids: Vec<PathBuf> = children.flatten().map(|c| c.path()).collect();
        kids.sort();
        for kid in kids {
            if is_real_dir(&kid) && is_real_file(&kid.join("index.json")) {
                out.push(SnapshotDir {
                    name: dir_name(&kid),
                    path: kid,
                });
            }
        }
    }
    out
}

impl Katalogr {
    /// Every stored point, oldest first, with no query and no wait. Over `limit`, the newest
    /// `limit` points, with a `Partial`. Never fails on an unreadable snapshot: it warns and skips.
    pub fn timeline(&self, limit: usize) -> Result<Reply<Timeline>, DistError> {
        let store = self.view_store();
        // The whole store is listed, not `limit` of it: snapshot rows replace repeated IDs, and
        // the cut is by commit time, which only the heads tell.
        let (heads, _) = store.list(usize::MAX);
        let mut points: BTreeMap<String, TimelinePoint> = BTreeMap::new();
        for h in heads {
            points.insert(
                h.view_id.clone(),
                TimelinePoint {
                    source: PointSource::StoredView,
                    view_id: Some(h.view_id),
                    query: Some(h.query),
                    positions: h.positions,
                    committed: None,
                    snapshot: None,
                },
            );
        }
        for snap in snapshot_dirs(&self.snapshot_dirs) {
            let index_path = snap.path.join("index.json");
            let index: SnapshotIndex = match std::fs::read(&index_path)
                .map_err(|e| e.to_string())
                .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
            {
                Ok(i) => i,
                Err(e) => {
                    self.note(format!("snapshot {} skipped: {e}", snap.name));
                    continue;
                }
            };
            let mut seen = BTreeSet::new();
            for block in &index.blocks {
                if !valid_id(&block.view_id) || !seen.insert(block.view_id.clone()) {
                    continue;
                }
                let file = snap.path.join(format!("{}.json", block.view_id));
                let head = if is_real_file(&file) {
                    std::fs::read(&file)
                        .ok()
                        .and_then(|b| parse_head(&b, &block.view_id))
                } else {
                    None
                };
                let Some(h) = head else {
                    self.note(format!(
                        "snapshot {}: view {} skipped, unreadable",
                        snap.name, block.view_id
                    ));
                    continue;
                };
                // A snapshot row says more than a stored one, so it replaces it.
                points.insert(
                    h.view_id.clone(),
                    TimelinePoint {
                        source: PointSource::Snapshot,
                        view_id: Some(h.view_id),
                        query: Some(h.query),
                        positions: h.positions,
                        committed: None,
                        snapshot: Some(snap.name.clone()),
                    },
                );
            }
        }
        let mut cache: BTreeMap<(String, String), Option<i64>> = BTreeMap::new();
        for p in points.values_mut() {
            p.committed = p
                .positions
                .iter()
                .filter_map(|pos| self.position_time(pos, &mut cache))
                .max();
        }
        let mut all: Vec<TimelinePoint> = points.into_values().collect();
        all.sort_by(|a, b| (a.committed, &a.view_id).cmp(&(b.committed, &b.view_id)));
        let total = all.len();
        let mut partial = Vec::new();
        if total > limit {
            all.drain(..total - limit);
            partial.push(Partial::new(
                cap::LIMIT,
                limit as u64,
                limit as u64,
                Some(total as u64),
            ));
        }
        Ok(Reply {
            outcome: Outcome::Found(Timeline { points: all }),
            report: self.stored_report(Vec::new()),
            diagnostic: None,
            partial,
            folded: Vec::new(),
        })
    }

    /// The stored bytes of `id` from the view store, else from the snapshot folders.
    /// A bad ID is a usage error. Absent is `Outcome::NoMatch`.
    pub fn stored_view(&self, id: &str) -> Result<Reply<ViewBytes>, DistError> {
        if !valid_id(id) {
            return Err(DistError::usage(format!("{id:?} is not a view ID")));
        }
        let in_store = self.view_store().get(id);
        let from_store = in_store.is_some();
        let found = in_store.or_else(|| {
            snapshot_dirs(&self.snapshot_dirs)
                .into_iter()
                .find_map(|s| {
                    let file = s.path.join(format!("{id}.json"));
                    if !is_real_file(&file) {
                        return None;
                    }
                    let bytes = std::fs::read(&file).ok()?;
                    parse_head(&bytes, id).map(|_| bytes)
                })
        });
        let (outcome, positions) = match found {
            Some(body) => {
                let positions = parse_head(&body, id)
                    .map(|h| h.positions)
                    .unwrap_or_default();
                (Outcome::Found(ViewBytes { body, from_store }), positions)
            }
            None => (Outcome::NoMatch(format!("no stored view {id}")), Vec::new()),
        };
        Ok(Reply {
            outcome,
            report: self.stored_report(positions),
            diagnostic: None,
            partial: Vec::new(),
            folded: Vec::new(),
        })
    }

    /// The report of a read that touched no index: nothing pending, and exact.
    fn stored_report(&self, positions: Vec<PositionRef>) -> Report {
        Report {
            watermark: 0,
            pending: 0,
            stale: false,
            timed_out: false,
            queued: 0,
            warnings: self.take_notes(),
            positions,
            working_set: None,
            exact: true,
            view_id: None,
            context: None,
        }
    }

    /// The committer time of a position's commit, through this project's roots, cached per
    /// commit for one call.
    fn position_time(
        &self,
        pos: &PositionRef,
        cache: &mut BTreeMap<(String, String), Option<i64>>,
    ) -> Option<i64> {
        let commit = pos.commit.as_ref()?;
        let key = (pos.repo.clone(), commit.clone());
        if let Some(t) = cache.get(&key) {
            return *t;
        }
        let t = self
            .roots
            .get(&pos.repo)
            .and_then(|root| katalogr_engine::commit_time::commit_time(root.as_std_path(), commit));
        cache.insert(key, t);
        t
    }
}
