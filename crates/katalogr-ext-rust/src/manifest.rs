//! `Cargo.toml` fragments (FEAT-005 section 4, "Manifests"): an artifact with the package
//! facts, a `crate:<name>` symbol, `depends-on` references and a crate-directory export.

use crate::context::{self, WorkspaceContext};
use katalogr_model::{
    ArtifactName, ArtifactNode, Confidence, Export, ExportKey, ExtractError, ExtractInput, Fact,
    Fragment, LinkKind, Name, OutRef, Provenance, Segment, Span, SymbolName, SymbolNode,
};
use std::collections::BTreeMap;
use toml::{Table, Value};

pub const NS_CRATE_DIR: &str = "rs-crate-dir";
pub const NS_PACKAGE: &str = "rs-package";

pub fn extract(input: &ExtractInput) -> Result<Fragment, ExtractError> {
    let text = std::str::from_utf8(input.content).map_err(|e| ExtractError::Parse {
        span: None,
        message: format!("not utf-8: {e}"),
    })?;
    let doc: Table = toml::from_str(text).map_err(|e| ExtractError::Parse {
        span: None,
        message: e.message().to_string(),
    })?;
    let ctx = WorkspaceContext::from_bytes(input.context)?;
    let dir = context::parent_dir(input.path.as_str());
    // Prefer the context (it resolves `workspace = true`); fall back to this file alone.
    let info = ctx
        .crate_at(dir)
        .cloned()
        .or_else(|| context::crate_from(dir, &doc, None));

    let artifact = ArtifactName {
        repo: input.repo.to_string(),
        path: input.path.clone(),
    };
    let provenance = Provenance {
        repo: input.repo.to_string(),
        path: input.path.clone(),
        revision: input.revision.to_string(),
    };
    let package = doc.get("package").and_then(Value::as_table);
    let mut facts: BTreeMap<String, Fact> = BTreeMap::new();
    for key in ["name", "version", "edition"] {
        if let Some(s) = package.and_then(|p| p.get(key)).and_then(Value::as_str) {
            facts.insert(key.to_string(), Fact::Str(s.to_string()));
        }
    }

    let mut symbols = Vec::new();
    let mut refs = Vec::new();
    let mut exports = Vec::new();
    if let Some(info) = info {
        let last = text.lines().last().unwrap_or("");
        let span = Span {
            start_line: 1,
            start_col: 1,
            end_line: text.lines().count().max(1) as u32,
            end_col: last.chars().count().max(1) as u32,
        };
        let name = SymbolName {
            artifact: artifact.clone(),
            segments: vec![Segment {
                kind: "crate".to_string(),
                name: info.name.clone(),
            }],
            ordinal: 1,
        };
        let id = name.id();
        symbols.push(SymbolNode {
            name,
            kind: "crate".to_string(),
            span,
            facts: BTreeMap::new(),
        });
        exports.push(Export {
            key: ExportKey {
                namespace: NS_CRATE_DIR.to_string(),
                key: context::key_dir(dir),
            },
            target: id,
        });
        exports.push(Export {
            key: ExportKey {
                namespace: NS_PACKAGE.to_string(),
                key: info.name.clone(),
            },
            target: id,
        });
        for dep in &info.deps {
            let candidate = match &dep.path {
                Some(p) => ExportKey {
                    namespace: NS_CRATE_DIR.to_string(),
                    key: context::key_dir(p),
                },
                None => ExportKey {
                    namespace: NS_PACKAGE.to_string(),
                    key: dep.package.clone(),
                },
            };
            refs.push(OutRef {
                from: id,
                kind: LinkKind("depends-on".to_string()),
                span: None,
                candidates: vec![candidate],
                confidence: Confidence::Extracted,
            });
        }
    }

    Ok(Fragment {
        provenance,
        extension: "rust".to_string(),
        artifact: ArtifactNode {
            name: artifact,
            facts,
        },
        symbols,
        links: Vec::new(),
        refs,
        exports,
        bindings: Vec::new(),
    })
}
