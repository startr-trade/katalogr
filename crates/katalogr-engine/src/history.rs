//! Commit history as fragments (FEAT-019 section 3, ADR-0016): every commit whose message
//! carries an item trailer becomes the artifact `<repo>:@history/<sha>`, with `refs-item`
//! references to the items it names and `touches` references to the files it changed. Also
//! blame, for `why`.

use crate::error::EngineError;
use gix::bstr::ByteSlice;
use katalogr_model::annotate;
use katalogr_model::{
    ArtifactName, ArtifactNode, Confidence, ExportKey, Fact, Fragment, LinkKind, Name, OutRef,
    Provenance, RelPath,
};
use std::collections::BTreeMap;
use std::path::Path;

/// Path prefix of history artifacts; the rest is the commit id.
pub const HISTORY_PREFIX: &str = "@history/";

/// Largest blob whose lines are diffed for the `hunks` fact.
const MAX_DIFF_BYTES: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryOptions {
    /// Bound on the commits examined by one walk (provisional; to be set from
    /// `bench::history_walk`).
    pub max_commits: usize,
    /// Message lines that start with one of these carry item references.
    pub trailer_prefixes: Vec<String>,
}

impl Default for HistoryOptions {
    fn default() -> Self {
        HistoryOptions {
            max_commits: 5000,
            trailer_prefixes: vec!["Refs:".to_string()],
        }
    }
}

fn err(e: impl std::fmt::Display) -> EngineError {
    EngineError::History(e.to_string())
}

fn open(root: &Path) -> Result<gix::Repository, EngineError> {
    gix::open(root).map_err(|e| EngineError::History(format!("cannot open repository: {e}")))
}

/// The item references the trailer lines of `message` name (the subject line is not a trailer).
fn trailer_refs(message: &str, o: &HistoryOptions) -> Vec<annotate::ItemRef> {
    let mut out: Vec<annotate::ItemRef> = Vec::new();
    for line in message.lines().skip(1) {
        let line = line.trim_start();
        if let Some(prefix) = o
            .trailer_prefixes
            .iter()
            .find(|p| line.starts_with(p.as_str()))
        {
            for r in annotate::scan(&line[prefix.len()..]) {
                if !out.contains(&r) {
                    out.push(r);
                }
            }
        }
    }
    out
}

/// Commit ids (hex) among all ancestors of HEAD (merged branches included, each commit once),
/// excluding `since` and its ancestors, whose message carries an item trailer. At most
/// `max_commits` commits are visited, newest first by commit time; the result is ordered by
/// commit time descending, then id.
pub fn history_shas(
    root: &Path,
    since: Option<&str>,
    o: &HistoryOptions,
) -> Result<Vec<String>, EngineError> {
    let repo = open(root)?;
    let head = repo.head_commit().map_err(err)?;
    let mut walk = head
        .ancestors()
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
        ));
    if let Some(rev) = since {
        let id = repo.rev_parse_single(rev).map_err(err)?;
        walk = walk.with_hidden([id.detach()]);
    }
    let mut found: Vec<(i64, String)> = Vec::new();
    for info in walk.all().map_err(err)?.take(o.max_commits) {
        let info = info.map_err(err)?;
        let commit = repo.find_commit(info.id).map_err(err)?;
        let message = commit
            .message_raw()
            .map_err(err)?
            .to_str_lossy()
            .into_owned();
        if !trailer_refs(&message, o).is_empty() {
            let time = commit
                .committer()
                .map_err(err)?
                .time()
                .map_err(err)?
                .seconds;
            found.push((time, info.id.to_string()));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(found.into_iter().map(|(_, sha)| sha).collect())
}

/// The fragment of one commit, or `None` when it is not in the repository or carries no item
/// trailer.
pub fn history_fragment(
    repo_name: &str,
    root: &Path,
    sha: &str,
    o: &HistoryOptions,
) -> Result<Option<Fragment>, EngineError> {
    let repo = open(root)?;
    let Ok(id) = gix::ObjectId::from_hex(sha.as_bytes()) else {
        return Ok(None);
    };
    let Ok(commit) = repo.find_commit(id) else {
        return Ok(None);
    };
    let message = commit
        .message_raw()
        .map_err(err)?
        .to_str_lossy()
        .into_owned();
    let items = trailer_refs(&message, o);
    if items.is_empty() {
        return Ok(None);
    }
    let path = RelPath::new(&format!("{HISTORY_PREFIX}{sha}")).map_err(err)?;
    let artifact = ArtifactName {
        repo: repo_name.to_string(),
        path: path.clone(),
    };
    let aid = artifact.id();

    let mut facts: BTreeMap<String, Fact> = BTreeMap::new();
    facts.insert(
        "summary".to_string(),
        Fact::Str(message.lines().next().unwrap_or("").trim().to_string()),
    );
    let author = commit.author().map_err(err)?;
    facts.insert(
        "author".to_string(),
        Fact::Str(author.name.to_str_lossy().into_owned()),
    );
    facts.insert(
        "date".to_string(),
        Fact::Str(iso_date(author.time().map_err(err)?.seconds)),
    );

    let mut refs = Vec::new();
    for r in &items {
        refs.push(OutRef {
            from: aid,
            kind: LinkKind("refs-item".to_string()),
            span: None,
            candidates: annotate::keys(r),
            confidence: Confidence::Extracted,
            origin: None,
            binds: None,
        });
    }
    let changed = changed_files(&repo, &commit)?;
    let mut hunks: BTreeMap<String, Fact> = BTreeMap::new();
    for (file, ranges) in &changed {
        refs.push(OutRef {
            from: aid,
            kind: LinkKind("touches".to_string()),
            span: None,
            candidates: vec![ExportKey {
                namespace: crate::stitch::FILE_NS.to_string(),
                key: format!("{repo_name}:{file}"),
            }],
            confidence: Confidence::Extracted,
            origin: None,
            binds: None,
        });
        hunks.insert(
            file.clone(),
            Fact::List(
                ranges
                    .iter()
                    .map(|(a, b)| Fact::Str(format!("{a}-{b}")))
                    .collect(),
            ),
        );
    }
    facts.insert("hunks".to_string(), Fact::Map(hunks));

    Ok(Some(Fragment {
        provenance: Provenance {
            repo: repo_name.to_string(),
            path,
            revision: sha.to_string(),
        },
        extension: "history".to_string(),
        artifact: ArtifactNode {
            name: artifact,
            facts,
        },
        symbols: Vec::new(),
        links: Vec::new(),
        refs,
        exports: Vec::new(),
        bindings: Vec::new(),
    }))
}

/// The fragments of every commit [`history_shas`] finds.
pub fn history_fragments(
    repo_name: &str,
    root: &Path,
    since: Option<&str>,
    o: &HistoryOptions,
) -> Result<Vec<Fragment>, EngineError> {
    let mut out = Vec::new();
    for sha in history_shas(root, since, o)? {
        out.extend(history_fragment(repo_name, root, &sha, o)?);
    }
    Ok(out)
}

/// Changed files of a commit against its first parent (the empty tree for a root commit), with
/// the new-file line ranges (1-based, inclusive) that changed. Sorted by path.
fn changed_files(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
) -> Result<BTreeMap<String, Vec<(u32, u32)>>, EngineError> {
    use gix::object::tree::diff::Change;
    let tree = commit.tree().map_err(err)?;
    let parent_tree = match commit.parent_ids().next() {
        Some(p) => repo.find_commit(p).map_err(err)?.tree().map_err(err)?,
        None => repo.empty_tree(),
    };
    let mut raw: Vec<(String, Option<gix::ObjectId>, gix::ObjectId)> = Vec::new();
    let mut platform = parent_tree.changes().map_err(err)?;
    platform.options(|o| {
        o.track_rewrites(None);
    });
    platform
        .for_each_to_obtain_tree(&tree, |change| {
            match change {
                Change::Addition {
                    location,
                    entry_mode,
                    id,
                    ..
                } if entry_mode.is_blob() => raw.push((location.to_string(), None, id.detach())),
                Change::Modification {
                    location,
                    entry_mode,
                    previous_id,
                    id,
                    ..
                } if entry_mode.is_blob() => raw.push((
                    location.to_string(),
                    Some(previous_id.detach()),
                    id.detach(),
                )),
                _ => {}
            }
            Ok(std::ops::ControlFlow::Continue(()))
        })
        .map_err(err)?;
    let mut out = BTreeMap::new();
    for (path, before, after) in raw {
        let new = repo.find_object(after).map_err(err)?;
        let old = match before {
            Some(b) => repo.find_object(b).map_err(err)?.data.clone(),
            None => Vec::new(),
        };
        out.insert(path, line_ranges(&old, &new.data));
    }
    Ok(out)
}

/// New-file line ranges that differ between `old` and `new`.
fn line_ranges(old: &[u8], new: &[u8]) -> Vec<(u32, u32)> {
    if old.len() > MAX_DIFF_BYTES || new.len() > MAX_DIFF_BYTES {
        return Vec::new();
    }
    let input = gix::diff::blob::InternedInput::new(old, new);
    let diff =
        gix::diff::blob::diff_with_slider_heuristics(gix::diff::blob::Algorithm::Histogram, &input);
    diff.hunks()
        .filter(|h| h.after.end > h.after.start)
        .map(|h| (h.after.start + 1, h.after.end))
        .collect()
}

/// `YYYY-MM-DD` (UTC) of a Unix time.
fn iso_date(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    // Civil-from-days (proleptic Gregorian), after Howard Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The commit (hex id) that introduced `line` (1-based) of `path` as of HEAD. Errors carry a
/// reason, for example a shallow clone or a file HEAD does not have.
pub fn blame_commit(root: &Path, path: &str, line: u32) -> Result<String, String> {
    let repo = gix::open(root).map_err(|e| format!("cannot open repository: {e}"))?;
    if repo.is_shallow().map_err(|e| e.to_string())? {
        return Err("shallow clone".to_string());
    }
    let head = repo.head_id().map_err(|e| format!("no HEAD: {e}"))?;
    let ranges = gix::blame::BlameRanges::from_one_based_inclusive_range(line..=line)
        .map_err(|e| e.to_string())?;
    let outcome = repo
        .blame_file(
            path.into(),
            head.detach(),
            gix::repository::blame_file::Options {
                ranges,
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?;
    let at = line - 1;
    outcome
        .entries
        .iter()
        .find(|e| e.start_in_blamed_file <= at && at < e.start_in_blamed_file + e.len.get())
        .map(|e| e.commit_id.to_string())
        .ok_or_else(|| format!("no blame entry for line {line}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(1_760_832_000), "2025-10-19");
        assert_eq!(iso_date(951_782_400), "2000-02-29");
    }

    #[test]
    fn trailers() {
        let o = HistoryOptions::default();
        let m =
            "subject FEAT-1\n\nbody FEAT-2\n\nRefs: kanbanr:FEAT-3/R-1, kanbanr:FEAT-4/TL-001/T2\n";
        let r = trailer_refs(m, &o);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].code, "FEAT-3");
        assert_eq!(r[1].task.as_deref(), Some("TL-001/T2"));
    }
}
