//! Replacing `katalogr-query` blocks in a document with diagrams (FEAT-032 R-7, R-8, R-11).
//!
//! A block whose query fails does not stop the document: it becomes a `pre.katalogr-error` box
//! holding the block's source and the error, and the failure is recorded for the caller.

use crate::{DistError, Outcome, Reply};
use katalogr_render::{
    PELE_INIT_JS, PELE_INIT_JS_NAME, PELE_JS, PELE_JS_NAME, PELE_THEME_CSS, PELE_THEME_CSS_NAME,
    ViewGraph, ViewSpec, replace_blocks, to_mermaid,
};
use std::path::Path;

/// A block that could not be drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockFailure {
    /// The exit code the same failure has as a command: 2 usage, 3 ambiguous, 4 no match, 5
    /// stale file, 1 anything else.
    pub code: i32,
    pub message: String,
}

/// A document after its blocks were replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocBlocks {
    pub text: String,
    /// Blocks replaced by a diagram.
    pub diagrams: usize,
    /// Blocks replaced by an error box, in document order.
    pub failures: Vec<BlockFailure>,
}

impl DocBlocks {
    /// How many query blocks the document held.
    pub fn blocks(&self) -> usize {
        self.diagrams + self.failures.len()
    }
}

/// Escape text for HTML content.
fn html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// The box that stands in for a block that failed. It holds no blank line, which would end the
/// HTML block in Markdown.
fn error_box(source: &str, message: &str) -> String {
    let collapse = |s: &str| {
        s.lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "<pre class=\"katalogr-error\"><span class=\"katalogr-error-message\">Diagram could not be drawn: {}</span>\n<code class=\"katalogr-error-source\">{}</code></pre>",
        html(&collapse(message)),
        html(&collapse(source)),
    )
}

/// A fenced Mermaid block whose fence is longer than any run of backticks in the text.
fn mermaid_fence(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    let body = text.trim_end_matches('\n');
    format!("{fence}mermaid\n{body}\n{fence}")
}

/// The failure an outcome other than an answer stands for.
fn failure_of(outcome: &Outcome<ViewGraph>) -> Option<BlockFailure> {
    match outcome {
        Outcome::Found(_) => None,
        Outcome::Ambiguous(c) => Some(BlockFailure {
            code: 3,
            message: format!("ambiguous: {} candidates", c.len()),
        }),
        Outcome::NoMatch(m) => Some(BlockFailure {
            code: 4,
            message: m.clone(),
        }),
        Outcome::StaleFile(m) => Some(BlockFailure {
            code: 5,
            message: m.clone(),
        }),
    }
}

/// Replaces each `katalogr-query` block of `markdown`. `view` answers one parsed block; it is
/// called once per block, in document order. `head`, when given, is written once, just before
/// the first block's replacement.
pub fn replace_doc_blocks(
    markdown: &str,
    head: Option<&str>,
    mut view: impl FnMut(&ViewSpec) -> Result<Reply<ViewGraph>, DistError>,
) -> DocBlocks {
    let mut diagrams = 0;
    let mut failures = Vec::new();
    let mut head = head;
    let text = replace_blocks(markdown, &mut |b| {
        let result = ViewSpec::parse(&b.source)
            .map_err(|e| BlockFailure {
                code: 2,
                message: e.to_string(),
            })
            .and_then(|spec| {
                view(&spec).map_err(|e| BlockFailure {
                    code: if e.is_usage() { 2 } else { 1 },
                    message: e.to_string(),
                })
            })
            .and_then(|reply| match failure_of(&reply.outcome) {
                Some(f) => Err(f),
                None => match reply.outcome {
                    Outcome::Found(g) => Ok(g),
                    _ => unreachable!("failure_of covers every other outcome"),
                },
            });
        let drawn = match result {
            Ok(g) => {
                diagrams += 1;
                mermaid_fence(&to_mermaid(&g))
            }
            Err(f) => {
                let boxed = error_box(&b.source, &f.message);
                failures.push(f);
                boxed
            }
        };
        match head.take() {
            Some(h) => format!("{}\n\n{drawn}", h.trim_end_matches('\n')),
            None => drawn,
        }
    });
    DocBlocks {
        text,
        diagrams,
        failures,
    }
}

/// Writes the three Pele files into `dir`, creating it, and rewrites only a file whose bytes
/// differ. Each write goes through a temporary file and a rename. Returns how many files were
/// written.
pub fn write_assets(dir: &Path) -> Result<usize, DistError> {
    std::fs::create_dir_all(dir).map_err(|e| DistError::io(dir.display(), e))?;
    let files: [(&str, &[u8]); 3] = [
        (PELE_THEME_CSS_NAME, PELE_THEME_CSS),
        (PELE_JS_NAME, PELE_JS),
        (PELE_INIT_JS_NAME, PELE_INIT_JS),
    ];
    let mut written = 0;
    for (name, bytes) in files {
        let target = dir.join(name);
        if std::fs::read(&target).is_ok_and(|have| have == bytes) {
            continue;
        }
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, bytes)
            .and_then(|()| std::fs::rename(&tmp, &target))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                DistError::io(target.display(), e)
            })?;
        written += 1;
    }
    Ok(written)
}
