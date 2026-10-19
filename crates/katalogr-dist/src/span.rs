//! `show` and `replace`: read or rewrite exactly one symbol's span (FEAT-025 sections 4 and 8).
//!
//! Both refuse when the file on disk is not the file that was indexed, so a wrong span is
//! never printed or overwritten.

use crate::error::DistError;
use crate::{Katalogr, Options, Outcome, Reply, unknown_root};
use katalogr_engine::{EventKind, NewEvent};
use katalogr_model::RelPath;
use katalogr_query::show::{Target, blob_of};
use katalogr_query::{ReplaceResult, ShowResult};
use katalogr_store::StoreError;
use katalogr_store::blob::git_blob_id;
use std::io::Write;
use std::path::PathBuf;

const STALE: &str = "the index is stale for this file; run `katalogr index`";

/// The bytes of the symbol's file, if they are the bytes that were indexed.
enum Read {
    Fresh(PathBuf, Vec<u8>),
    Stale(String),
}

/// Byte offsets of the start of every line, and the end of the text.
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    starts
}

/// Byte range of lines `first..=last` (1-based, inclusive), including the last line's
/// terminator when it has one. `None` when the lines are outside the text.
fn line_range(text: &str, first: u32, last: u32) -> Option<(usize, usize)> {
    let starts = line_starts(text);
    // A trailing newline starts an empty final "line" that is not a real line.
    let real = if text.ends_with('\n') {
        starts.len() - 1
    } else {
        starts.len()
    };
    if first == 0 || first > last || last as usize > real {
        return None;
    }
    let start = starts[first as usize - 1];
    let end = starts.get(last as usize).copied().unwrap_or(text.len());
    Some((start, end))
}

impl Katalogr {
    fn read_fresh(&self, t: &Target) -> Result<Read, DistError> {
        let root = self
            .roots
            .get(&t.repo)
            .ok_or_else(|| unknown_root(&t.repo))?;
        let rel = RelPath::new(&t.symbol.path).map_err(|e| DistError::usage(e.to_string()))?;
        let file = root.join(rel.as_str());
        let canon_root = std::fs::canonicalize(root).map_err(|e| DistError::io(root, e))?;
        let meta = match std::fs::symlink_metadata(&file) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Read::Stale(STALE.to_string()));
            }
            Err(e) => return Err(DistError::io(&file, e)),
        };
        if meta.file_type().is_symlink() {
            return Err(DistError::usage(format!(
                "{file}: refusing to follow a symlink"
            )));
        }
        let canon = std::fs::canonicalize(&file).map_err(|e| DistError::io(&file, e))?;
        if !canon.starts_with(&canon_root) {
            return Err(DistError::usage(format!(
                "{file}: not inside the repo root"
            )));
        }
        let bytes = std::fs::read(&canon).map_err(|e| DistError::io(&file, e))?;
        if git_blob_id(&bytes) != blob_of(&t.revision) {
            return Ok(Read::Stale(STALE.to_string()));
        }
        Ok(Read::Fresh(canon, bytes))
    }

    /// The span of the symbol `text` names, or with `signature_only` its `signature` and `doc`
    /// facts. The file must be the one that was indexed (else exit 5 via
    /// [`Outcome::StaleFile`]).
    pub fn show(
        &self,
        text: &str,
        signature_only: bool,
        opts: &Options,
    ) -> Result<Reply<ShowResult>, DistError> {
        let reply = self.query(opts, |tx| {
            crate::on_target(tx, text, |s| {
                katalogr_query::show::target(tx, s)?
                    .ok_or_else(|| StoreError::Row(format!("{} is no longer indexed", s.name)))
            })
        })?;
        let outcome = match reply.outcome {
            Outcome::Found(t) => self.show_target(&t, signature_only)?,
            Outcome::Ambiguous(c) => Outcome::Ambiguous(c),
            Outcome::NoMatch(m) => Outcome::NoMatch(m),
            Outcome::StaleFile(m) => Outcome::StaleFile(m),
        };
        Ok(Reply {
            outcome,
            report: reply.report,
            diagnostic: None,
        })
    }

    fn show_target(
        &self,
        t: &Target,
        signature_only: bool,
    ) -> Result<Outcome<ShowResult>, DistError> {
        let span = t.symbol.span;
        let lines = (span.start_line, span.end_line);
        let body = if signature_only {
            [t.signature.as_deref(), t.doc.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            let (_, bytes) = match self.read_fresh(t)? {
                Read::Fresh(p, b) => (p, b),
                Read::Stale(m) => return Ok(Outcome::StaleFile(m)),
            };
            let text = String::from_utf8_lossy(&bytes);
            let Some((from, to)) = line_range(&text, lines.0, lines.1) else {
                return Ok(Outcome::StaleFile(STALE.to_string()));
            };
            text[from..to]
                .trim_end_matches(['\n', '\r'])
                .replace("\r\n", "\n")
        };
        Ok(Outcome::Found(ShowResult {
            symbol: t.symbol.clone(),
            file: t.symbol.path.clone(),
            lines,
            text: body,
        }))
    }

    /// Replace exactly the line range of the symbol `text` names with the content of
    /// `new_body`, keeping the file's line endings; write a temp file, fsync, rename, then
    /// queue the file for reindexing. Never reformats. Refuses an ambiguous name (exit 3) and a
    /// file that is not the one indexed (exit 5).
    pub fn replace(
        &self,
        text: &str,
        new_body: &std::path::Path,
        opts: &Options,
    ) -> Result<Reply<ReplaceResult>, DistError> {
        let body = std::fs::read(new_body).map_err(|e| DistError::io(new_body.display(), e))?;
        let body = String::from_utf8(body)
            .map_err(|_| DistError::usage(format!("{}: not UTF-8", new_body.display())))?;
        if body.trim().is_empty() {
            return Err(DistError::usage("the replacement is empty"));
        }
        let reply = self.query(opts, |tx| {
            crate::on_target(tx, text, |s| {
                katalogr_query::show::target(tx, s)?
                    .ok_or_else(|| StoreError::Row(format!("{} is no longer indexed", s.name)))
            })
        })?;
        let outcome = match reply.outcome {
            Outcome::Found(t) => self.replace_target(&t, &body)?,
            Outcome::Ambiguous(c) => Outcome::Ambiguous(c),
            Outcome::NoMatch(m) => Outcome::NoMatch(m),
            Outcome::StaleFile(m) => Outcome::StaleFile(m),
        };
        Ok(Reply {
            outcome,
            report: reply.report,
            diagnostic: None,
        })
    }

    fn replace_target(&self, t: &Target, body: &str) -> Result<Outcome<ReplaceResult>, DistError> {
        let (path, bytes) = match self.read_fresh(t)? {
            Read::Fresh(p, b) => (p, b),
            Read::Stale(m) => return Ok(Outcome::StaleFile(m)),
        };
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| DistError::usage(format!("{}: not UTF-8", t.symbol.path)))?;
        let span = t.symbol.span;
        let Some((from, to)) = line_range(text, span.start_line, span.end_line) else {
            return Ok(Outcome::StaleFile(STALE.to_string()));
        };
        let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let had_terminator = text[from..to].ends_with('\n');
        let new_lines: Vec<&str> = body.lines().collect();
        let mut block = new_lines.join(eol);
        if had_terminator {
            block.push_str(eol);
        }
        let mut out = String::with_capacity(text.len() + block.len());
        out.push_str(&text[..from]);
        out.push_str(&block);
        out.push_str(&text[to..]);

        let dir = path
            .parent()
            .ok_or_else(|| DistError::usage("file has no directory"))?;
        let mut tmp =
            tempfile::NamedTempFile::new_in(dir).map_err(|e| DistError::io(dir.display(), e))?;
        tmp.write_all(out.as_bytes())
            .map_err(|e| DistError::io(path.display(), e))?;
        tmp.as_file()
            .sync_all()
            .map_err(|e| DistError::io(path.display(), e))?;
        if let Ok(meta) = std::fs::metadata(&path) {
            let _ = std::fs::set_permissions(tmp.path(), meta.permissions());
        }
        // The file must still be the one that was read.
        let now = std::fs::read(&path).map_err(|e| DistError::io(path.display(), e))?;
        if git_blob_id(&now) != git_blob_id(&bytes) {
            return Ok(Outcome::StaleFile(STALE.to_string()));
        }
        tmp.persist(&path)
            .map_err(|e| DistError::io(path.display(), e.error))?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }

        let rel = RelPath::new(&t.symbol.path).map_err(|e| DistError::usage(e.to_string()))?;
        katalogr_engine::queue::Inbox::new(&self.project).append(&NewEvent::file(
            &t.repo,
            &rel,
            EventKind::Changed,
        ))?;
        let count = new_lines.len() as u32;
        Ok(Outcome::Found(ReplaceResult {
            symbol: t.symbol.clone(),
            file: t.symbol.path.clone(),
            replaced: (span.start_line, span.end_line),
            lines: (span.start_line, span.start_line + count - 1),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::line_range;

    #[test]
    fn line_ranges() {
        let t = "a\nbb\nccc\n";
        assert_eq!(line_range(t, 1, 1), Some((0, 2)));
        assert_eq!(line_range(t, 2, 3), Some((2, 9)));
        assert_eq!(line_range(t, 3, 4), None);
        assert_eq!(line_range(t, 0, 1), None);
        let u = "a\nbb";
        assert_eq!(line_range(u, 2, 2), Some((2, 4)));
    }
}
