//! The splitter: headings else blank-line blocks, capped in lines, chunks and bytes.

use crate::{MAX_CHUNK_LINES, MAX_CHUNKS, MAX_TEXT_BYTES};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// 1-based.
    pub start_line: u32,
    /// Inclusive; the last non-blank line of the chunk.
    pub end_line: u32,
    /// Level 1..=6 and text, for the chunk that begins at a heading.
    pub heading: Option<(u8, String)>,
    /// The chunk's lines joined by `\n`, each right-trimmed, cut to `MAX_TEXT_BYTES`.
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Headings,
    Blocks,
}

#[derive(Clone, Debug)]
pub struct Split {
    pub chunks: Vec<Chunk>,
    pub mode: Mode,
    /// `MAX_CHUNKS` was reached and more chunks followed.
    pub truncated: bool,
}

/// The lines of `text`; a final empty line after a trailing `\n` is not a line.
pub fn lines_of(text: &str) -> Vec<&str> {
    let mut v: Vec<&str> = text.split('\n').collect();
    if v.last() == Some(&"") {
        v.pop();
    }
    v
}

fn is_blank(l: &str) -> bool {
    l.trim().is_empty()
}

fn heading_of(line: &str) -> Option<(u8, String)> {
    let spaces = line.bytes().take_while(|&b| b == b' ').count();
    if spaces > 3 {
        return None;
    }
    let rest = &line[spaces..];
    let hashes = rest.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let after = &rest[hashes..];
    let first = after.chars().next()?;
    if first != ' ' && first != '\t' {
        return None;
    }
    if after.trim().is_empty() {
        return None;
    }
    let title = after
        .trim()
        .trim_end_matches('#')
        .trim_end_matches([' ', '\t'])
        .to_string();
    Some((hashes as u8, title))
}

/// Cut `s` to at most `max` bytes on a char boundary.
pub fn cut(s: &mut String, max: usize) {
    if s.len() > max {
        let mut n = max;
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        s.truncate(n);
    }
}

/// Split decoded text into chunks. Pure and deterministic.
pub fn split(text: &str) -> Split {
    let lines = lines_of(text);
    // Heading per line, ignoring fenced code.
    let mut in_fence = false;
    let mut headings: Vec<Option<(u8, String)>> = Vec::with_capacity(lines.len());
    for l in &lines {
        let t = l.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            headings.push(None);
        } else if in_fence {
            headings.push(None);
        } else {
            headings.push(heading_of(l));
        }
    }
    let mode = if headings.iter().any(Option::is_some) {
        Mode::Headings
    } else {
        Mode::Blocks
    };
    // Ranges of 0-based line indexes, half-open, with the heading that opens them.
    let mut ranges: Vec<(usize, usize, Option<(u8, String)>)> = Vec::new();
    match mode {
        Mode::Headings => {
            let mut start = 0;
            let mut head: Option<(u8, String)> = None;
            for (i, h) in headings.iter().enumerate() {
                if let Some(h) = h {
                    ranges.push((start, i, head.take()));
                    start = i;
                    head = Some(h.clone());
                }
            }
            ranges.push((start, lines.len(), head));
        }
        Mode::Blocks => {
            let mut i = 0;
            while i < lines.len() {
                if is_blank(lines[i]) {
                    i += 1;
                    continue;
                }
                let s = i;
                while i < lines.len() && !is_blank(lines[i]) {
                    i += 1;
                }
                ranges.push((s, i, None));
            }
        }
    }
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut truncated = false;
    'outer: for (s, e, head) in ranges {
        let mut head = head;
        let mut p = s;
        while p < e {
            let q = (p + MAX_CHUNK_LINES).min(e);
            let piece = &lines[p..q];
            let (Some(first), Some(last)) = (
                piece.iter().position(|l| !is_blank(l)),
                piece.iter().rposition(|l| !is_blank(l)),
            ) else {
                p = q;
                continue;
            };
            if chunks.len() >= MAX_CHUNKS {
                truncated = true;
                break 'outer;
            }
            let mut body = piece[first..=last]
                .iter()
                .map(|l| l.trim_end())
                .collect::<Vec<_>>()
                .join("\n");
            cut(&mut body, MAX_TEXT_BYTES);
            chunks.push(Chunk {
                start_line: (p + first) as u32 + 1,
                end_line: (p + last) as u32 + 1,
                heading: head.take(),
                text: body,
            });
            p = q;
        }
    }
    Split {
        chunks,
        mode,
        truncated,
    }
}
