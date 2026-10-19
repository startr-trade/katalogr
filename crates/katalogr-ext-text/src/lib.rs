//! Text fallback extension (FEAT-066, ADR-0020): files that a profile `select` routes here
//! become an artifact with section or line-block chunks, searchable by BM25.
//!
//! The extension claims no file by itself.

pub mod chunk;
pub mod decode;
mod lines;

use katalogr_model::{
    ArtifactName, ArtifactNode, ExtractError, ExtractInput, Extractor, Fact, Fragment, Provenance,
    RelPath, Segment, SymbolName, SymbolNode,
};
use std::collections::BTreeMap;

/// Larger files get an artifact only.
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
/// A chunk spans at most this many lines.
pub const MAX_CHUNK_LINES: usize = 60;
/// Chunks per file.
pub const MAX_CHUNKS: usize = 5_000;
/// The `text` fact of a chunk, cut on a char boundary.
pub const MAX_TEXT_BYTES: usize = 2_048;
pub const BINARY_SNIFF_BYTES: usize = 8_192;

const TITLE_CHARS: usize = 120;

#[derive(Clone, Copy, Debug, Default)]
pub struct TextExtractor;

impl Extractor for TextExtractor {
    fn id(&self) -> &'static str {
        "text"
    }

    /// Always false: files are routed here only by a profile `select`.
    fn handles(&self, _: &RelPath) -> bool {
        false
    }

    fn patterns(&self) -> &'static [&'static str] {
        &["chunks", "headings", "blocks", "binary", "encoding", "size-cap"]
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        self.extract_with_errors(input).map(|(f, _)| f)
    }

    /// Never returns `Err`: every input yields an artifact; problems are recoverable errors.
    fn extract_with_errors(
        &self,
        input: &ExtractInput,
    ) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
        let artifact = ArtifactName {
            repo: input.repo.to_string(),
            path: input.path.clone(),
        };
        let provenance = Provenance {
            repo: input.repo.to_string(),
            path: input.path.clone(),
            revision: input.revision.to_string(),
        };
        let bytes = input.content;
        let mut facts: BTreeMap<String, Fact> = BTreeMap::new();
        facts.insert("bytes".to_string(), Fact::Int(bytes.len() as i64));
        let mut errors = Vec::new();
        let mut symbols = Vec::new();

        let skipped = if bytes.len() > MAX_INPUT_BYTES {
            errors.push(ExtractError::Unsupported(
                "larger than 1 MiB; not chunked".to_string(),
            ));
            Some("too-large")
        } else if decode::is_binary(bytes) {
            errors.push(ExtractError::Unsupported(
                "binary content; not chunked".to_string(),
            ));
            Some("binary")
        } else {
            None
        };
        if let Some(reason) = skipped {
            facts.insert("skipped".to_string(), Fact::Str(reason.to_string()));
        } else {
            let (text, enc, msg) = decode::decode(bytes);
            if let Some(message) = msg {
                errors.push(ExtractError::Parse {
                    span: None,
                    message,
                });
            }
            facts.insert("encoding".to_string(), Fact::Str(enc.as_str().to_string()));
            let line_list = chunk::lines_of(&text);
            facts.insert("lines".to_string(), Fact::Int(line_list.len() as i64));
            let split = chunk::split(&text);
            let title = split
                .chunks
                .iter()
                .find_map(|c| c.heading.as_ref().map(|(_, t)| t.clone()))
                .or_else(|| {
                    line_list
                        .iter()
                        .find(|l| !l.trim().is_empty())
                        .map(|l| l.trim().chars().take(TITLE_CHARS).collect())
                });
            if let Some(t) = title {
                facts.insert("title".to_string(), Fact::Str(t));
            }
            facts.insert(
                "chunk_mode".to_string(),
                Fact::Str(
                    match split.mode {
                        chunk::Mode::Headings => "headings",
                        chunk::Mode::Blocks => "blocks",
                    }
                    .to_string(),
                ),
            );
            if split.truncated {
                errors.push(ExtractError::Unsupported(
                    "chunk limit of 5000 reached; the rest is not indexed".to_string(),
                ));
            }
            let ls = lines::Lines::new(&text);
            let mut offsets = Vec::with_capacity(line_list.len() + 1);
            let mut at = 0usize;
            for l in &line_list {
                offsets.push(at);
                at += l.len() + 1;
            }
            offsets.push(at);
            for c in &split.chunks {
                let start = offsets[c.start_line as usize - 1];
                // End of the last line, exclusive of its newline.
                let end = offsets[c.end_line as usize - 1] + line_list[c.end_line as usize - 1].len();
                let mut f = BTreeMap::new();
                f.insert("text".to_string(), Fact::Str(c.text.clone()));
                f.insert(
                    "lines".to_string(),
                    Fact::Int((c.end_line - c.start_line + 1) as i64),
                );
                if let Some((level, title)) = &c.heading {
                    f.insert("title".to_string(), Fact::Str(title.clone()));
                    f.insert("level".to_string(), Fact::Int(*level as i64));
                }
                symbols.push(SymbolNode {
                    name: SymbolName {
                        artifact: artifact.clone(),
                        segments: vec![Segment {
                            kind: "chunk".to_string(),
                            name: format!("l{}", c.start_line),
                        }],
                        ordinal: 1,
                    },
                    kind: "chunk".to_string(),
                    span: ls.span(start as u32, end.max(start + 1) as u32),
                    facts: f,
                });
            }
            symbols.sort_by(|a, b| a.name.cmp(&b.name));
        }
        Ok((
            Fragment {
                provenance,
                extension: self.id().to_string(),
                artifact: ArtifactNode {
                    name: artifact,
                    facts,
                },
                symbols,
                links: Vec::new(),
                refs: Vec::new(),
                exports: Vec::new(),
                bindings: Vec::new(),
            },
            errors,
        ))
    }
}
