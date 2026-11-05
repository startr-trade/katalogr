//! Query blocks in documents, checked and snapshotted (FEAT-035).
//!
//! [`check_blocks`] parses every block under a path and reads no index. [`Katalogr::snapshot`]
//! answers every distinct block at one position and writes the answers into a folder the
//! caller names, all or nothing. Neither writes to a document or to a board.

use crate::doc_blocks::mermaid_fence;
use crate::{DistError, Katalogr, Options, Outcome};
use katalogr_query::PositionRef;
use katalogr_render::{ViewSpec, find_blocks};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A query block that does not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockProblem {
    /// Relative to the checked folder, with `/` separators; the file name for a single file.
    pub file: String,
    /// The line of the block's opening fence, counted from 1.
    pub line: u32,
    pub source: String,
    pub error: String,
}

/// What a snapshot is asked to do.
#[derive(Clone, Debug)]
pub struct SnapshotRequest {
    pub docs: PathBuf,
    pub out: PathBuf,
    pub options: Options,
}

/// `index.json`: the positions, the repos named, and each block with its view ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SnapshotIndex {
    pub schema: u32,
    /// Sorted, without repeats.
    pub positions: Vec<PositionRef>,
    /// Every repo named by a position or in a view's body, sorted.
    pub repos: Vec<String>,
    /// Sorted by document, then line.
    pub blocks: Vec<SnapshotBlock>,
}

/// One block of a document and the view that answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SnapshotBlock {
    /// Relative to `--docs`.
    pub doc: String,
    pub line: u32,
    pub source: String,
    pub view_id: String,
    pub partial: bool,
}

/// A block found in a document.
struct Found {
    doc: String,
    line: u32,
    source: String,
}

/// `.md` files under `root` (or `root` itself, when it is a file), sorted by relative path,
/// skipping hidden folders and files.
fn walk_markdown(root: &Path) -> Result<Vec<(String, PathBuf)>, DistError> {
    let meta = std::fs::metadata(root).map_err(|e| DistError::io(root.display(), e))?;
    let mut out = Vec::new();
    if meta.is_file() {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push((name, root.to_path_buf()));
        return Ok(out);
    }
    fn go(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) -> Result<(), DistError> {
        let rd = std::fs::read_dir(dir).map_err(|e| DistError::io(dir.display(), e))?;
        for entry in rd {
            let entry = entry.map_err(|e| DistError::io(dir.display(), e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let rel_path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let ty = entry
                .file_type()
                .map_err(|e| DistError::io(path.display(), e))?;
            if ty.is_dir() {
                go(&path, &rel_path, out)?;
            } else if ty.is_file() && name.ends_with(".md") {
                out.push((rel_path, path));
            }
        }
        Ok(())
    }
    go(root, "", &mut out)?;
    out.sort();
    Ok(out)
}

/// Every block of every document under `root`, in order, with its problem if it does not parse.
/// A block and its parse result.
type Parsed = (Found, Result<ViewSpec, String>);

fn blocks_under(root: &Path) -> Result<Vec<Parsed>, DistError> {
    let mut all = Vec::new();
    for (doc, path) in walk_markdown(root)? {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            // A file that is not UTF-8 text holds no block we can read.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(e) => return Err(DistError::io(path.display(), e)),
        };
        for b in find_blocks(&text) {
            let line = text[..b.start].bytes().filter(|c| *c == b'\n').count() as u32 + 1;
            let spec = ViewSpec::parse(&b.source).map_err(|e| e.to_string());
            all.push((
                Found {
                    doc: doc.clone(),
                    line,
                    source: b.source,
                },
                spec,
            ));
        }
    }
    Ok(all)
}

fn problems_of(blocks: &[Parsed]) -> Vec<BlockProblem> {
    blocks
        .iter()
        .filter_map(|(f, r)| {
            r.as_ref().err().map(|e| BlockProblem {
                file: f.doc.clone(),
                line: f.line,
                source: f.source.clone(),
                error: e.clone(),
            })
        })
        .collect()
}

/// Parses every query block in the `.md` files under `root` (a folder or one file). Reads no
/// index, so a document check can call it without a project.
pub fn check_blocks(root: &Path) -> Result<Vec<BlockProblem>, DistError> {
    Ok(problems_of(&blocks_under(root)?))
}

/// The key two blocks asking one question share.
type Key = (String, BTreeMap<String, String>);

/// One answered question.
struct Answer {
    body: Vec<u8>,
    view_id: String,
    markdown: String,
    partial: bool,
    positions: Vec<PositionRef>,
    repos: BTreeSet<String>,
}

fn refused(exit: i32, message: String) -> DistError {
    DistError::Refused { exit, message }
}

/// The answer's fields, or why it cannot be written.
fn read_answer(body: Vec<u8>, partial_reported: bool) -> Result<Answer, String> {
    let v: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("the view answer is not JSON: {e}"))?;
    if v["exact"] != serde_json::Value::Bool(true) {
        return Err("the view is not exact: it has no ID and is not stored".to_string());
    }
    let view_id = v["view_id"]
        .as_str()
        .ok_or("the exact view has no ID")?
        .to_string();
    let positions: Vec<PositionRef> = serde_json::from_value(v["positions"].clone())
        .map_err(|e| format!("the view answer has bad positions: {e}"))?;
    let mut repos: BTreeSet<String> = positions.iter().map(|p| p.repo.clone()).collect();
    let nodes = v["graph"]["nodes"].as_array().cloned().unwrap_or_default();
    for n in &nodes {
        if let Some(g) = n["group"].as_str() {
            repos.insert(g.to_string());
        }
    }
    let shown = v["graph"]["shown"].as_u64().unwrap_or(0);
    let total = v["graph"]["total"].as_u64().unwrap_or(0);
    let partial = partial_reported || total > shown;
    let title = v["graph"]["title"].as_str().unwrap_or_default();
    let mermaid = v["mermaid"].as_str().unwrap_or_default();
    let mut markdown = format!(
        "---\nview_id: {view_id}\nquery: {}\npartial: {partial}\n---\n\n{}\n",
        serde_json::to_string(title).expect("serialise a string"),
        mermaid_fence(mermaid),
    );
    if partial {
        markdown.push_str("\nThis view is partial: a cap cut it.\n");
    }
    Ok(Answer {
        body,
        view_id,
        markdown,
        partial,
        positions,
        repos,
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), DistError> {
    std::fs::write(path, bytes).map_err(|e| DistError::io(path.display(), e))
}

/// The folder next to `out` that holds a run's files until the rename.
fn temp_prefix(out: &Path) -> String {
    let name = out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "snapshot".to_string());
    format!(".{name}.snapshot-tmp-")
}

impl Katalogr {
    /// Answers every distinct query block under `req.docs` at `req.options` and writes the
    /// answers to `req.out`. All or nothing: a block that does not parse, or a view that is not
    /// exact or not available, ends the run with nothing written. `req.out` must not exist or
    /// must be empty.
    pub fn snapshot(&self, req: &SnapshotRequest) -> Result<SnapshotIndex, DistError> {
        let blocks = blocks_under(&req.docs)?;
        let bad = problems_of(&blocks);
        if !bad.is_empty() {
            let lines: Vec<String> = bad
                .iter()
                .map(|p| format!("{}:{}: {}", p.file, p.line, p.error))
                .collect();
            return Err(DistError::usage(format!(
                "{} query block(s) do not parse:\n{}",
                bad.len(),
                lines.join("\n")
            )));
        }
        if let Ok(rd) = std::fs::read_dir(&req.out) {
            if rd.count() > 0 {
                return Err(DistError::usage(format!(
                    "{} exists and is not empty",
                    req.out.display()
                )));
            }
        } else if req.out.exists() {
            return Err(DistError::usage(format!(
                "{} exists and is not a folder",
                req.out.display()
            )));
        }

        // One view per distinct question.
        let mut specs: BTreeMap<Key, ViewSpec> = BTreeMap::new();
        for (_, spec) in &blocks {
            let spec = spec.as_ref().expect("parse errors returned above");
            specs
                .entry((spec.op.clone(), spec.args.clone()))
                .or_insert_with(|| spec.clone());
        }
        let mut answers: BTreeMap<Key, Answer> = BTreeMap::new();
        let mut failures: Vec<(Key, i32, String)> = Vec::new();
        for (key, spec) in &specs {
            let reply = self.view(spec, &req.options)?;
            let reported_partial = !reply.partial.is_empty();
            match reply.outcome {
                Outcome::Found(b) => match read_answer(b.body, reported_partial) {
                    Ok(a) => {
                        answers.insert(key.clone(), a);
                    }
                    Err(m) => failures.push((key.clone(), 5, m)),
                },
                Outcome::Ambiguous(c) => {
                    failures.push((key.clone(), 3, format!("ambiguous: {} candidates", c.len())));
                }
                Outcome::NoMatch(m) => failures.push((key.clone(), 4, m)),
                Outcome::StaleFile(m) => failures.push((key.clone(), 5, m)),
                Outcome::PositionUnavailable { wanted, .. } => failures.push((
                    key.clone(),
                    6,
                    format!(
                        "no context holds position {wanted}; for a release position, run `context add` at that commit first"
                    ),
                )),
            }
        }
        if !failures.is_empty() {
            let mut lines = Vec::new();
            for (key, _, message) in &failures {
                for (f, spec) in &blocks {
                    let s = spec.as_ref().expect("parsed");
                    if (&s.op, &s.args) == (&key.0, &key.1) {
                        lines.push(format!("{}:{}: {}: {message}", f.doc, f.line, f.source));
                    }
                }
            }
            lines.sort();
            // The first block in document order sets the status.
            let code = blocks
                .iter()
                .find_map(|(_, s)| {
                    let s = s.as_ref().expect("parsed");
                    failures
                        .iter()
                        .find(|(k, _, _)| (&k.0, &k.1) == (&s.op, &s.args))
                        .map(|(_, c, _)| *c)
                })
                .unwrap_or(1);
            return Err(refused(
                code,
                format!("snapshot refused, nothing written:\n{}", lines.join("\n")),
            ));
        }

        let mut positions: BTreeSet<PositionRef> = BTreeSet::new();
        let mut repos: BTreeSet<String> = BTreeSet::new();
        for a in answers.values() {
            positions.extend(a.positions.iter().cloned());
            repos.extend(a.repos.iter().cloned());
        }
        let mut index = SnapshotIndex {
            schema: 1,
            positions: positions.into_iter().collect(),
            repos: repos.into_iter().collect(),
            blocks: blocks
                .iter()
                .map(|(f, s)| {
                    let s = s.as_ref().expect("parsed");
                    let a = &answers[&(s.op.clone(), s.args.clone())];
                    SnapshotBlock {
                        doc: f.doc.clone(),
                        line: f.line,
                        source: f.source.clone(),
                        view_id: a.view_id.clone(),
                        partial: a.partial,
                    }
                })
                .collect(),
        };
        index
            .blocks
            .sort_by(|a, b| (&a.doc, a.line).cmp(&(&b.doc, b.line)));

        // Write beside `out`, then rename, so the folder appears whole or not at all.
        let parent = match req.out.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        std::fs::create_dir_all(&parent).map_err(|e| DistError::io(parent.display(), e))?;
        let prefix = temp_prefix(&req.out);
        if let Ok(rd) = std::fs::read_dir(&parent) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        let tmp = parent.join(format!("{prefix}{}", std::process::id()));
        let written = (|| {
            std::fs::create_dir_all(&tmp).map_err(|e| DistError::io(tmp.display(), e))?;
            for a in answers.values() {
                write_file(&tmp.join(format!("{}.json", a.view_id)), &a.body)?;
                write_file(
                    &tmp.join(format!("{}.md", a.view_id)),
                    a.markdown.as_bytes(),
                )?;
            }
            let mut json = serde_json::to_string_pretty(&index).expect("serialise the index");
            json.push('\n');
            write_file(&tmp.join("index.json"), json.as_bytes())?;
            if req.out.is_dir() {
                std::fs::remove_dir(&req.out).map_err(|e| DistError::io(req.out.display(), e))?;
            }
            std::fs::rename(&tmp, &req.out).map_err(|e| DistError::io(req.out.display(), e))
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        Ok(index)
    }
}
