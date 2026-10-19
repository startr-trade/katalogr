//! Privacy lint: denied terms are stored only as SHA-256 hashes (FEAT-023 section 11).

use camino::Utf8Path;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use crate::AccError;

/// SHA-256 of `token`.
pub fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Lowercase hex of a digest.
pub fn to_hex(h: &[u8; 32]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Read the deny list: one lowercase hex digest per line; blank lines are ignored.
pub fn load_denied(path: &Utf8Path) -> Result<BTreeSet<[u8; 32]>, AccError> {
    let text = std::fs::read_to_string(path).map_err(|e| AccError::io(path, e))?;
    let mut set = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let h = from_hex(l).ok_or_else(|| AccError::Format {
            path: path.to_string(),
            message: format!("line {}: not a SHA-256 hex digest", i + 1),
        })?;
        set.insert(h);
    }
    Ok(set)
}

/// Lowercase tokens: maximal runs of `[a-z0-9]`, plus every path-like run `[a-z0-9._/-]+` and
/// its `/`-components.
pub fn tokenize(text: &str) -> BTreeSet<String> {
    let lower = text.to_lowercase();
    let mut out = BTreeSet::new();
    let mut run = String::new();
    for c in lower.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            run.push(c);
        } else if !run.is_empty() {
            out.insert(std::mem::take(&mut run));
        }
    }
    let mut path = String::new();
    for c in lower.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '/' | '-') {
            path.push(c);
        } else if !path.is_empty() {
            for comp in path.split('/').filter(|s| !s.is_empty()) {
                out.insert(comp.to_string());
            }
            out.insert(std::mem::take(&mut path));
        }
    }
    out
}

/// Any token whose SHA-256 is in `denied` gives an entry `file:line` (never the matched term).
/// Non-UTF-8 bytes are read lossily.
pub fn lint_file(path: &Utf8Path, denied: &BTreeSet<[u8; 32]>) -> Result<(), Vec<String>> {
    let bytes = std::fs::read(path).map_err(|e| vec![format!("{path}: unreadable: {e}")])?;
    let text = String::from_utf8_lossy(&bytes);
    let hits: Vec<String> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| tokenize(line).iter().any(|t| denied.contains(&hash(t))))
        .map(|(i, _)| format!("{path}:{}", i + 1))
        .collect();
    if hits.is_empty() { Ok(()) } else { Err(hits) }
}

/// Lint every file under `dir` (recursively, sorted); the deny list file itself is skipped.
pub fn lint_tree(dir: &Utf8Path, denied: &BTreeSet<[u8; 32]>) -> Vec<String> {
    let mut failures = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return failures;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(p) = camino::Utf8PathBuf::from_path_buf(e.path()) else {
            continue;
        };
        if p.is_dir() {
            failures.extend(lint_tree(&p, denied));
        } else if p.file_name() != Some("privacy.sha256")
            && let Err(f) = lint_file(&p, denied)
        {
            failures.extend(f);
        }
    }
    failures
}
