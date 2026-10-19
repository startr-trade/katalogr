//! Markdown extension (FEAT-019, ADR-0016): sections and links of `*.md` and `*.markdown`.

mod lines;
pub mod markdown;

use katalogr_model::{
    ArtifactName, ArtifactNode, ExtractError, ExtractInput, Extractor, Fact, Fragment, Provenance,
    RelPath,
};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default)]
pub struct MarkdownExtractor;

impl Extractor for MarkdownExtractor {
    fn id(&self) -> &'static str {
        "md"
    }

    fn handles(&self, p: &RelPath) -> bool {
        let s = p.as_str();
        s.ends_with(".md") || s.ends_with(".markdown")
    }

    fn patterns(&self) -> &'static [&'static str] {
        &["sections", "links", "dangling-links", "annotations"]
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        let text = std::str::from_utf8(input.content).map_err(|e| ExtractError::Parse {
            span: None,
            message: format!("not utf-8: {e}"),
        })?;
        let parsed = markdown::parse(input.repo, input.path, input.revision, text);
        let artifact = ArtifactName {
            repo: input.repo.to_string(),
            path: input.path.clone(),
        };
        let mut facts: BTreeMap<String, Fact> = BTreeMap::new();
        if let Some(t) = &parsed.title {
            facts.insert("title".to_string(), Fact::Str(t.clone()));
        }
        if !parsed.external.is_empty() {
            facts.insert(
                "urls".to_string(),
                Fact::List(parsed.external.iter().cloned().map(Fact::Str).collect()),
            );
        }
        Ok(Fragment {
            provenance: Provenance {
                repo: input.repo.to_string(),
                path: input.path.clone(),
                revision: input.revision.to_string(),
            },
            extension: "md".to_string(),
            artifact: ArtifactNode {
                name: artifact,
                facts,
            },
            symbols: parsed.symbols,
            links: parsed.links,
            refs: parsed.refs,
            exports: parsed.exports,
        })
    }
}
