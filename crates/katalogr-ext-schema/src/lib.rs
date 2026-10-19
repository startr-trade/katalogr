//! Schemas (FEAT-009): XSD and JSON Schema (JSON and YAML, and the component schemas of API
//! documents) as types, elements and fields, with the contracts `schema_type` and
//! `schema_element` so that mirrored copies of a schema can be compared.
//!
//! Nothing here fetches a URL, processes a DTD or an external entity, or expands a YAML alias.

pub mod build;
pub mod jsonschema;
mod paths;
pub mod qname;
mod xsd;

use katalogr_ext_data::tree::{self, Parsed};
use katalogr_ext_data::{Format, read_text};
use katalogr_model::{ExtractError, ExtractInput, Extractor, Fragment, RelPath};

fn ends_with_ci(path: &RelPath, suffixes: &[&str]) -> bool {
    let p = path.as_str().to_ascii_lowercase();
    suffixes.iter().any(|s| p.ends_with(s))
}

/// XML Schema files: `*.xsd`.
#[derive(Clone, Copy, Debug, Default)]
pub struct XsdExtractor;

impl Extractor for XsdExtractor {
    fn id(&self) -> &'static str {
        "xsd"
    }

    fn handles(&self, path: &RelPath) -> bool {
        ends_with_ci(path, &[".xsd"])
    }

    fn patterns(&self) -> &'static [&'static str] {
        &[
            "types",
            "fields",
            "refs",
            "imports",
            "contracts",
            "parse-error",
        ]
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        self.extract_with_errors(input).map(|(f, _)| f)
    }

    fn extract_with_errors(
        &self,
        input: &ExtractInput,
    ) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
        let text = read_text(input)?;
        let parsed = tree::parse_xml(text);
        Ok(xsd::extract(input, text, parsed))
    }
}

/// JSON Schema files: `*.schema.json`, `*.schema.yaml`, `*.schema.yml`, and any `.json`,
/// `.yaml` or `.yml` file that a profile routes to this extension.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsonSchemaExtractor;

impl Extractor for JsonSchemaExtractor {
    fn id(&self) -> &'static str {
        "json-schema"
    }

    fn handles(&self, path: &RelPath) -> bool {
        ends_with_ci(path, &[".schema.json", ".schema.yaml", ".schema.yml"])
    }

    fn patterns(&self) -> &'static [&'static str] {
        &[
            "defs",
            "properties",
            "refs",
            "yaml",
            "api-components",
            "parse-error",
        ]
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        self.extract_with_errors(input).map(|(f, _)| f)
    }

    fn extract_with_errors(
        &self,
        input: &ExtractInput,
    ) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
        let format = match katalogr_ext_data::format_of(input.path) {
            Some(f @ (Format::Json | Format::Yaml)) => f,
            _ => {
                return Err(ExtractError::Unsupported(format!(
                    "no JSON or YAML format for {}",
                    input.path
                )));
            }
        };
        let text = read_text(input)?;
        let Parsed { docs, mut errors } = katalogr_ext_data::parse(format, text);
        let mut b = build::Builder::new(input, "json-schema", text);
        b.artifact_fact(
            "schema_kind",
            katalogr_model::Fact::Str("json-schema".to_string()),
        );
        if docs.len() > 1 {
            errors.push(ExtractError::Parse {
                span: Some(docs[1].span),
                message: "only the first document is read".to_string(),
            });
        }
        if let Some(doc) = docs.first() {
            errors.extend(jsonschema::extract_schemas(
                &mut b, doc, input.repo, input.path,
            ));
        }
        Ok((b.finish(), errors))
    }
}
