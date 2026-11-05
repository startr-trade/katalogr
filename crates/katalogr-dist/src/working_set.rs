//! Working-set inputs gathered by the composition root (FEAT-039).
//!
//! The edit log is the "recent" input: `<context>/.cache/recent-edits`, one repo-relative path
//! per line, newest last. It is a hint, not a record: two writers may race and the last rename
//! wins, which costs at most a boost.

use crate::Katalogr;
use katalogr_engine::base::{branch_tip, default_branch, head_branch, merge_base};
use katalogr_engine::position::{read_position_cached, stat_cache_path};
use katalogr_engine::scan::{PathChange, changed_paths};
use katalogr_model::annotate::ITEM_NS;
use katalogr_model::{ArtifactName, ExportKey, Name, RelPath};
use katalogr_query::{Tier, Weights, WorkingSet};
use katalogr_store::{ReadTx, Reader};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// File name of the edit log inside the context's cache directory.
pub const EDIT_LOG: &str = "recent-edits";
/// Default for `working_set.recent_max` (provisional; FEAT-039 T7 sweeps it).
pub const DEFAULT_RECENT_MAX: usize = 32;

/// The paths in the edit log, oldest first. A missing or unreadable log is empty, and lines that
/// are not valid repo-relative paths are skipped.
pub fn read_edits(cache_dir: &Path) -> Vec<RelPath> {
    std::fs::read_to_string(cache_dir.join(EDIT_LOG))
        .map(|s| s.lines().filter_map(|l| RelPath::new(l).ok()).collect())
        .unwrap_or_default()
}

/// Append `path` as the newest edit: any earlier line for it is dropped, only the last `max`
/// paths are kept, and the file is written temp-then-rename.
pub fn append_edit(cache_dir: &Path, path: &RelPath, max: usize) -> std::io::Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let mut edits = read_edits(cache_dir);
    edits.retain(|p| p != path);
    edits.push(path.clone());
    if edits.len() > max {
        edits.drain(..edits.len() - max);
    }
    let mut text = String::new();
    for p in &edits {
        text.push_str(p.as_str());
        text.push('\n');
    }
    let tmp = cache_dir.join(format!("{EDIT_LOG}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, cache_dir.join(EDIT_LOG)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Most paths the `Item` tier holds; over that, the first by path.
pub const ITEM_PATH_CAP: usize = 500;
/// Default for `working_set.item_pattern`.
pub const DEFAULT_ITEM_PATTERN: &str = "[A-Z]+-[0-9]+";
/// Default for `working_set.item_kinds`.
pub const DEFAULT_ITEM_KINDS: [&str; 2] = ["refs-item", "annotated-by"];
/// Paths of commit artifacts, which are not files and never join the working set.
const HISTORY_PREFIX: &str = "@history/";

impl Katalogr {
    fn ws_setting(&self, key: &str) -> Option<&serde_yaml_ng::Value> {
        self.project.config().settings.get(key)
    }

    /// The boost weights: `working_set.weight.<tier>` over the defaults.
    pub fn working_set_weights(&self) -> Weights {
        let d = Weights::default();
        let get = |tier: &str, default: f64| {
            self.ws_setting(&format!("working_set.weight.{tier}"))
                .and_then(serde_yaml_ng::Value::as_f64)
                .unwrap_or(default)
        };
        Weights {
            dirty: get("dirty", d.dirty),
            recent: get("recent", d.recent),
            branch: get("branch", d.branch),
            item: get("item", d.item),
        }
    }

    /// The cache directory of this invocation's context.
    fn ws_cache_dir(&self) -> PathBuf {
        let dir = self
            .layer_dir(Instant::now() + Duration::from_secs(5))
            .unwrap_or_else(|_| self.project.clone());
        dir.cache_dir().into_std_path_buf()
    }

    /// The item named by the branch, from `working_set.item_pattern` (first match on the
    /// branch's short name), when the graph holds it through the `board-item` namespace. No
    /// board indexed, no branch or no match is `None`, never an error.
    pub fn active_item(&self, tx: &ReadTx) -> Option<String> {
        let (_, root) = self.current().ok()?;
        let branch = head_branch(root.as_std_path())?;
        self.item_of_branch(&branch, tx)
    }

    fn item_of_branch(&self, branch: &str, tx: &ReadTx) -> Option<String> {
        let pattern = self
            .ws_setting("working_set.item_pattern")
            .and_then(serde_yaml_ng::Value::as_str)
            .unwrap_or(DEFAULT_ITEM_PATTERN);
        let re = regex::Regex::new(pattern).ok()?;
        let code = re.find(branch)?.as_str().to_string();
        let key = ExportKey {
            namespace: ITEM_NS.to_string(),
            key: code.clone(),
        };
        tx.exports(&key)
            .ok()
            .filter(|ids| !ids.is_empty())
            .map(|_| code)
    }

    fn item_kinds(&self) -> Vec<String> {
        self.ws_setting("working_set.item_kinds")
            .and_then(serde_yaml_ng::Value::as_sequence)
            .map(|s| {
                s.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_else(|| DEFAULT_ITEM_KINDS.map(str::to_string).to_vec())
    }

    /// Dirty paths, the branch diff from the merge-base with the default branch, recent edits
    /// from the log, and paths linked to the active item. Never fails a read: a failed input is
    /// empty, with a warning. A path takes its strongest tier only. The checkout's position must
    /// have a commit for the `Dirty` tier to apply: a directory with no history has no "changed".
    pub fn working_set(&self, tx: &ReadTx) -> (WorkingSet, Vec<String>) {
        self.working_set_with(tx).0
    }

    /// [`Katalogr::working_set`] with the active item, for the answer's summary.
    pub(crate) fn working_set_with(
        &self,
        tx: &ReadTx,
    ) -> ((WorkingSet, Vec<String>), Option<String>) {
        let mut ws = WorkingSet::default();
        let mut warnings = Vec::new();
        let Ok((repo, root)) = self.current() else {
            return ((ws, warnings), None);
        };
        let root = root.as_std_path();
        let cache = self.ws_cache_dir();

        // Dirty: what the position reads as changed.
        let (include, exclude) = self
            .project
            .config()
            .repos
            .iter()
            .find(|r| r.name == repo)
            .map_or((Vec::new(), Vec::new()), |r| {
                (r.include.clone(), r.exclude.clone())
            });
        let mut head_commit = None;
        match read_position_cached(
            repo,
            root,
            &include,
            &exclude,
            Some(&stat_cache_path(&cache, repo)),
        ) {
            Ok(state) => {
                head_commit = state.position.commit.clone();
                if head_commit.is_some() {
                    for (p, _) in state.changed {
                        ws.add(repo, p, Tier::Dirty);
                    }
                }
            }
            Err(e) => warnings.push(format!("working set: dirty paths unavailable: {e}")),
        }

        // Recent: the edit log.
        for p in read_edits(&cache) {
            ws.add(repo, p, Tier::Recent);
        }

        // Branch: the diff from the merge-base with the default branch.
        if let Some(head) = head_commit {
            let configured = self.base_branch_setting(repo);
            if let Ok(Some(def)) = default_branch(root, configured.as_deref())
                && let Ok(Some(tip)) = branch_tip(root, &def)
                && let Ok(Some(base)) = merge_base(root, &tip, &head)
                && base != head
            {
                match changed_paths(root, &format!("{base}..{head}")) {
                    Ok(changes) => {
                        for c in changes {
                            match c {
                                PathChange::Added(p) | PathChange::Modified(p) => {
                                    ws.add(repo, p, Tier::Branch);
                                }
                                PathChange::Deleted(_) => {}
                                PathChange::Renamed { to, .. } => ws.add(repo, to, Tier::Branch),
                            }
                        }
                    }
                    Err(e) => warnings.push(format!("working set: branch diff unavailable: {e}")),
                }
            }
        }

        // Item: paths holding a symbol linked to the active item.
        let item = head_branch(root).and_then(|b| self.item_of_branch(&b, tx));
        if let Some(code) = &item {
            match self.item_paths(tx, code) {
                Ok((paths, partial)) => {
                    for (r, p) in paths {
                        ws.add(&r, p, Tier::Item);
                    }
                    if partial {
                        warnings.push(format!(
                            "partial: working set holds the first {ITEM_PATH_CAP} paths linked to {code}"
                        ));
                    }
                }
                Err(e) => warnings.push(format!("working set: item paths unavailable: {e}")),
            }
        }
        // Only paths the graph holds can be ranked; the rest (a marker file, a path no
        // extension reads, a deleted file) would only make the answer name a set it cannot use.
        ws.paths.retain(|(r, p), _| {
            let id = ArtifactName {
                repo: r.clone(),
                path: p.clone(),
            }
            .id();
            tx.artifact(id).ok().flatten().is_some()
        });
        ((ws, warnings), item)
    }

    /// The `(repo, path)` of artifacts holding a symbol linked to `code` by the item kinds, the
    /// first [`ITEM_PATH_CAP`] by path, and whether the cap cut the list.
    fn item_paths(
        &self,
        tx: &ReadTx,
        code: &str,
    ) -> Result<(Vec<(String, RelPath)>, bool), katalogr_store::StoreError> {
        let kinds = self.item_kinds();
        let key = ExportKey {
            namespace: ITEM_NS.to_string(),
            key: code.to_string(),
        };
        let mut found: BTreeSet<(String, RelPath)> = BTreeSet::new();
        for target in tx.exports(&key)? {
            for link in tx.links_in(target)? {
                if !kinds.contains(&link.kind.0) {
                    continue;
                }
                let name: Option<ArtifactName> = match tx.symbol(link.from)? {
                    Some(s) => tx.artifact(s.artifact)?.map(|a| a.name),
                    None => tx.artifact(link.from)?.map(|a| a.name),
                };
                if let Some(n) = name
                    && !n.path.as_str().starts_with(HISTORY_PREFIX)
                {
                    found.insert((n.repo, n.path));
                }
            }
        }
        let partial = found.len() > ITEM_PATH_CAP;
        Ok((found.into_iter().take(ITEM_PATH_CAP).collect(), partial))
    }
}
