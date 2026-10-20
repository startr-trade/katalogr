//! OpenAPI extension (FEAT-014): the operations of OpenAPI 3.x and Swagger 2.0 documents as
//! symbols declaring `http_route` contracts, the key normaliser that makes every route key
//! byte-identical, and a guest for request-line literals (ADR-0021).
//!
//! The extension claims only conventionally named files (`openapi*`, `swagger*`, `*.openapi`,
//! `*.swagger` with a `.yaml`, `.yml` or `.json` suffix); any other location is routed by a
//! profile `select`. HTTP framework conventions are profile rules, not code (ADR-0002).

pub mod doc;
pub mod embedded;
pub mod route;

use katalogr_ext_data::tree::{self, Parsed};
use katalogr_ext_schema::build::Builder;
use katalogr_ext_schema::jsonschema;
use katalogr_model::{
    Binding, Confidence, ContractRole, EmbeddedInput, ExtractError, ExtractInput, Extractor, Fact,
    Fragment, KeyNormaliser, RelPath,
};

/// Files larger than this are not parsed.
pub const OPENAPI_MAX_BYTES: usize = 16 * 1024 * 1024;

static NORMALISERS: [&dyn KeyNormaliser; 1] = [&route::HTTP_ROUTE];

#[derive(Clone, Copy, Debug, Default)]
pub struct OpenApiExtractor;

/// True for a conventional OpenAPI file name: the last path component, ASCII-lowercased, ends in
/// `.yaml`, `.yml` or `.json`, and its stem starts with `openapi` or `swagger`, or ends with
/// `.openapi` or `.swagger`.
pub fn conventional_name(path: &RelPath) -> bool {
    let name = path
        .as_str()
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let Some(stem) = [".yaml", ".yml", ".json"]
        .iter()
        .find_map(|s| name.strip_suffix(s))
    else {
        return false;
    };
    stem.starts_with("openapi")
        || stem.starts_with("swagger")
        || stem.ends_with(".openapi")
        || stem.ends_with(".swagger")
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

impl Extractor for OpenApiExtractor {
    fn id(&self) -> &'static str {
        "openapi"
    }

    fn handles(&self, path: &RelPath) -> bool {
        conventional_name(path)
    }

    fn patterns(&self) -> &'static [&'static str] {
        &[
            "operations",
            "swagger2",
            "component-schemas",
            "plain-artifact",
            "parse-error",
        ]
    }

    fn key_normalisers(&self) -> &'static [&'static dyn KeyNormaliser] {
        &NORMALISERS
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        self.extract_with_errors(input).map(|(f, _)| f)
    }

    fn extract_with_errors(
        &self,
        input: &ExtractInput,
    ) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
        extract(input)
    }

    fn embedded_version(&self) -> Option<&'static str> {
        Some("1")
    }

    fn accepts_embedded(&self, text: &str) -> bool {
        embedded::accepts(text)
    }

    fn extract_embedded(&self, input: &EmbeddedInput) -> Vec<Binding> {
        embedded::extract(input)
    }
}

/// The fragment of a file that holds no OpenAPI document: the artifact alone.
fn plain(input: &ExtractInput) -> Fragment {
    Builder::new(input, "openapi", "").finish()
}

fn extract(input: &ExtractInput) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
    if input.content.len() > OPENAPI_MAX_BYTES {
        return Ok((
            plain(input),
            vec![ExtractError::Parse {
                span: None,
                message: "larger than 16 MiB; not parsed".to_string(),
            }],
        ));
    }
    let text = std::str::from_utf8(input.content).map_err(|e| ExtractError::Parse {
        span: None,
        message: format!("not utf-8: {e}"),
    })?;
    // A document that names neither marker is not worth a parse.
    if !contains(input.content, b"openapi") && !contains(input.content, b"swagger") {
        return Ok((plain(input), Vec::new()));
    }
    let json = input.path.as_str().to_ascii_lowercase().ends_with(".json");
    let Parsed { docs, mut errors } = if json {
        tree::parse_json(text)
    } else {
        tree::parse_yaml(text)
    };
    let Some(root) = docs.first() else {
        return Ok((plain(input), errors));
    };
    let Some(version) = doc::kind(root) else {
        return Ok((plain(input), errors));
    };
    if docs.len() > 1 {
        errors.push(ExtractError::Parse {
            span: Some(docs[1].span),
            message: "only the first document is read".to_string(),
        });
    }
    let mut b = Builder::new(input, "openapi", text);
    if let Some(v) = doc::version_text(root) {
        b.artifact_fact("openapi_version", Fact::Str(v.to_string()));
    }
    if let Some(t) = root
        .get("info")
        .and_then(|i| i.get("title"))
        .and_then(|t| t.as_text())
    {
        b.artifact_fact("title", Fact::Str(t.to_string()));
    }
    b.artifact_fact("base_path", Fact::Str(doc::root_base(version, root)));
    for op in doc::operations(root, version) {
        let sym = b.symbol(
            None,
            "operation",
            &format!("{} {}", op.method, op.path),
            op.span,
            op.facts,
        );
        if let Some(key) = op.route {
            b.bind(
                sym,
                route::CONTRACT_KIND,
                key,
                ContractRole::Declares,
                op.key_span,
                Confidence::Extracted,
            );
        }
    }
    errors.extend(jsonschema::extract_schemas(
        &mut b, root, input.repo, input.path,
    ));
    Ok((b.finish(), errors))
}
