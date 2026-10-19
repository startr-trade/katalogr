//! One file to one [`Fragment`] (FEAT-006 sections 7 and 8). Source files are parsed with oxc;
//! a syntax error never discards the file: the recovered program is extracted and the errors
//! are recorded on the artifact in the fact `parse_errors`.

use crate::context::{self, PACKAGE_JSON, TsContext};
use crate::items::Builder;
use crate::modules::{NS_FILE, NS_PACKAGE, key};
use katalogr_model::{
    ArtifactName, ArtifactNode, Export, ExtractError, ExtractInput, Fact, Fragment, Name,
    Provenance, Segment, Span, SymbolName, SymbolNode,
};
use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;
use std::collections::BTreeMap;
use std::path::Path;

pub fn extract(input: &ExtractInput) -> Result<Fragment, ExtractError> {
    let text = std::str::from_utf8(input.content).map_err(|e| ExtractError::Parse {
        span: None,
        message: format!("not utf-8: {e}"),
    })?;
    let ctx = TsContext::from_bytes(input.context)?;
    match context::file_name(input.path.as_str()) {
        PACKAGE_JSON | context::TSCONFIG | context::JSCONFIG => Ok(config_fragment(input, text)),
        _ => source_fragment(input, text, &ctx),
    }
}

fn error_fact(line: u32, col: u32, message: &str) -> Fact {
    Fact::Map(BTreeMap::from([
        ("col".to_string(), Fact::Int(col as i64)),
        ("line".to_string(), Fact::Int(line as i64)),
        ("message".to_string(), Fact::Str(message.to_string())),
    ]))
}

fn source_fragment(
    input: &ExtractInput,
    text: &str,
    ctx: &TsContext,
) -> Result<Fragment, ExtractError> {
    let source_type = SourceType::from_path(Path::new(input.path.as_str()))
        .map_err(|_| ExtractError::Unsupported(format!("file kind of {}", input.path)))?;
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, text, source_type).parse();
    let mut b = Builder::new(ctx, input.repo, input.path, input.revision, text);

    // Imports are hoisted, so their bindings come first.
    for stmt in &ret.program.body {
        if let Some(oxc_ast::ast::ModuleDeclaration::ImportDeclaration(d)) =
            stmt.as_module_declaration()
        {
            b.import_declaration(d);
        } else if let Some(oxc_ast::ast::Declaration::VariableDeclaration(v)) =
            stmt.as_declaration()
        {
            b.require_bindings(v);
        }
    }
    let mut pending = Vec::new();
    for stmt in &ret.program.body {
        if matches!(
            stmt.as_module_declaration(),
            Some(oxc_ast::ast::ModuleDeclaration::ImportDeclaration(_))
        ) {
            continue;
        }
        b.declare_statement(stmt, &mut pending);
    }
    b.finish_exports();
    b.walk_calls(pending);

    let mut facts = BTreeMap::new();
    let errors: Vec<Fact> = ret
        .diagnostics
        .errors()
        .map(|d| {
            let offset = d.labels.first().map_or(0, |l| l.offset());
            let (line, col) = b.lines.position(offset as usize);
            error_fact(line, col, &d.message)
        })
        .collect();
    if !errors.is_empty() {
        facts.insert("parse_errors".to_string(), Fact::List(errors));
    }
    let _ = &ret.module_record;

    let mut symbols = std::mem::take(&mut b.symbols);
    symbols.sort_by_key(|s| s.name.canonical());
    Ok(Fragment {
        provenance: b.provenance.clone(),
        extension: "ts".to_string(),
        artifact: ArtifactNode {
            name: b.artifact.clone(),
            facts,
        },
        symbols,
        links: std::mem::take(&mut b.links),
        refs: std::mem::take(&mut b.refs),
        exports: std::mem::take(&mut b.exports),
    })
}

/// `tsconfig.json`, `jsconfig.json`, `package.json`: an artifact; `package.json` also a
/// `package:<name>` symbol that other files reach through `("ts-package", <name>)`.
fn config_fragment(input: &ExtractInput, text: &str) -> Fragment {
    let artifact = ArtifactName {
        repo: input.repo.to_string(),
        path: input.path.clone(),
    };
    let mut exports = vec![Export {
        key: key(NS_FILE, input.path.as_str().to_string()),
        target: artifact.id(),
    }];
    let mut facts = BTreeMap::new();
    let mut symbols = Vec::new();
    match context::parse_jsonc(input.content) {
        None => {
            let (line, col, msg) =
                match serde_json::from_str::<serde_json::Value>(&context::strip_jsonc(text)) {
                    Err(e) => (e.line() as u32, e.column() as u32, e.to_string()),
                    Ok(_) => (1, 1, "not valid JSON".to_string()),
                };
            facts.insert(
                "parse_errors".to_string(),
                Fact::List(vec![error_fact(line.max(1), col.max(1), &msg)]),
            );
        }
        Some(doc) => {
            if context::file_name(input.path.as_str()) == PACKAGE_JSON
                && let Some(name) = doc.get("name").and_then(|n| n.as_str())
            {
                facts.insert("name".to_string(), Fact::Str(name.to_string()));
                let lines = crate::lines::Lines::new(text);
                let span: Span = lines.span(0, text.len() as u32);
                let sname = SymbolName {
                    artifact: artifact.clone(),
                    segments: vec![Segment {
                        kind: "package".to_string(),
                        name: name.to_string(),
                    }],
                    ordinal: 1,
                };
                exports.push(Export {
                    key: key(NS_PACKAGE, name.to_string()),
                    target: sname.id(),
                });
                symbols.push(SymbolNode {
                    name: sname,
                    kind: "package".to_string(),
                    span,
                    facts: BTreeMap::new(),
                });
            }
        }
    }
    Fragment {
        provenance: Provenance {
            repo: input.repo.to_string(),
            path: input.path.clone(),
            revision: input.revision.to_string(),
        },
        extension: "ts".to_string(),
        artifact: ArtifactNode {
            name: artifact,
            facts,
        },
        symbols,
        links: Vec::new(),
        refs: Vec::new(),
        exports,
    }
}
