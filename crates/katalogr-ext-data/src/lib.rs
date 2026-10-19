//! Data documents (FEAT-009): JSON, YAML and XML files indexed as a key tree of symbols.
//!
//! The extension claims no file by itself. A project profile routes files to it with a `select`
//! entry, and its rules then give the tree its meaning.

pub mod keytree;
pub mod lines;
pub mod tree;

use katalogr_model::{
    ArtifactName, ArtifactNode, ExtractError, ExtractInput, Extractor, Fact, Fragment, Provenance,
    RelPath,
};
use std::collections::BTreeMap;

/// Files larger than this are not read.
pub const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;
/// Key and item symbols per file; the rest of a larger file is not indexed.
pub const MAX_SYMBOLS: usize = 20_000;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Yaml,
    Xml,
}

/// By the path's suffix, ASCII case-insensitive: `.json`, `.yaml`/`.yml`, `.xml`.
pub fn format_of(path: &RelPath) -> Option<Format> {
    let name = path.as_str().to_ascii_lowercase();
    if name.ends_with(".json") {
        Some(Format::Json)
    } else if name.ends_with(".yaml") || name.ends_with(".yml") {
        Some(Format::Yaml)
    } else if name.ends_with(".xml") {
        Some(Format::Xml)
    } else {
        None
    }
}

/// Parse `text` in `format` into documents (YAML may hold several; JSON and XML hold one).
pub fn parse(format: Format, text: &str) -> tree::Parsed {
    match format {
        Format::Json => tree::parse_json(text),
        Format::Yaml => tree::parse_yaml(text),
        Format::Xml => tree::parse_xml(text),
    }
}

/// The text of a file within the size bound, or the reason it is not read.
pub fn read_text<'a>(input: &ExtractInput<'a>) -> Result<&'a str, ExtractError> {
    if input.content.len() > MAX_INPUT_BYTES {
        return Err(ExtractError::Unsupported("larger than 8 MiB".to_string()));
    }
    std::str::from_utf8(input.content).map_err(|e| ExtractError::Parse {
        span: None,
        message: format!("not utf-8: {e}"),
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DataExtractor;

impl Extractor for DataExtractor {
    fn id(&self) -> &'static str {
        "data"
    }

    /// Always false: data documents are routed to this extension only by a profile `select`.
    fn handles(&self, _: &RelPath) -> bool {
        false
    }

    fn patterns(&self) -> &'static [&'static str] {
        &["key-tree", "yaml", "xml", "parse-error"]
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        self.extract_with_errors(input).map(|(f, _)| f)
    }

    fn extract_with_errors(
        &self,
        input: &ExtractInput,
    ) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
        let format = format_of(input.path).ok_or_else(|| {
            ExtractError::Unsupported(format!("no data format for {}", input.path))
        })?;
        let text = read_text(input)?;
        let mut parsed = parse(format, text);
        let artifact = ArtifactName {
            repo: input.repo.to_string(),
            path: input.path.clone(),
        };
        let provenance = Provenance {
            repo: input.repo.to_string(),
            path: input.path.clone(),
            revision: input.revision.to_string(),
        };
        let file = input.path.as_str().rsplit('/').next().unwrap_or("");
        let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
        let mut b = keytree::KeyTreeBuilder::new(artifact.clone(), provenance.clone());
        for doc in &parsed.docs {
            keytree::emit(&mut b, doc, stem, format == Format::Xml);
        }
        b.finish_symbols();
        let mut facts = BTreeMap::new();
        facts.insert(
            "data_format".to_string(),
            Fact::Str(
                match format {
                    Format::Json => "json",
                    Format::Yaml => "yaml",
                    Format::Xml => "xml",
                }
                .to_string(),
            ),
        );
        if b.truncated {
            facts.insert("truncated".to_string(), Fact::Bool(true));
            parsed.errors.push(ExtractError::Parse {
                span: None,
                message: format!("more than {MAX_SYMBOLS} keys; the rest is not indexed"),
            });
        }
        Ok((
            Fragment {
                provenance,
                extension: self.id().to_string(),
                artifact: ArtifactNode {
                    name: artifact,
                    facts,
                },
                symbols: b.symbols,
                links: b.links,
                refs: Vec::new(),
                exports: Vec::new(),
                bindings: Vec::new(),
            },
            parsed.errors,
        ))
    }
}
