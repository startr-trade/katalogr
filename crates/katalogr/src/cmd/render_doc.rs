//! `katalogr render mdbook` and `katalogr render filter`: the doc-tool modes of `render`.
//!
//! Both replace `katalogr-query` blocks with Mermaid and add the Pele script and theme. A block
//! whose query fails becomes an error box and the run goes on with exit 0; `filter --strict`
//! exits with the first failure's code once the output is written.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use katalogr_dist::doc_blocks::{BlockFailure, DocBlocks, replace_doc_blocks, write_assets};
use katalogr_dist::render::{ViewGraph, ViewSpec, find_blocks, head_tags};
use katalogr_dist::{DistError, Katalogr, Reply};
use serde_json::Value;

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;

/// The directory under the book's source the assets are written to, and linked from.
const ASSETS_DIR: &str = "katalogr-assets";

/// The project, resolved when the first block needs it and not before: a document with no
/// block needs no project.
struct Lazy<'a> {
    cli: &'a Cli,
    katalogr: Option<Result<Katalogr, String>>,
}

impl<'a> Lazy<'a> {
    fn new(cli: &'a Cli) -> Self {
        Lazy {
            cli,
            katalogr: None,
        }
    }

    fn view(&mut self, spec: &ViewSpec) -> Result<Reply<ViewGraph>, DistError> {
        let cli = self.cli;
        let k = self
            .katalogr
            .get_or_insert_with(|| resolve(cli).map_err(|e| e.to_string()));
        match k {
            Ok(k) => k.view_graph(spec, &options(cli)),
            Err(m) => Err(DistError::Usage(m.clone())),
        }
    }
}

fn exit_of(code: i32) -> ExitCode {
    match code {
        2 => ExitCode::Usage,
        3 => ExitCode::Ambiguous,
        4 => ExitCode::NoMatch,
        5 => ExitCode::Stale,
        _ => ExitCode::Error,
    }
}

fn warn(failures: &[BlockFailure]) {
    for f in failures {
        eprintln!("katalogr: warning: query block not drawn: {}", f.message);
    }
}

fn read_stdin() -> Result<Vec<u8>, DistError> {
    let mut buf = Vec::new();
    std::io::stdin()
        .read_to_end(&mut buf)
        .map_err(|e| DistError::Io {
            path: "stdin".to_string(),
            message: e.to_string(),
        })?;
    Ok(buf)
}

fn write_stdout(bytes: &[u8]) -> Result<(), DistError> {
    let mut out = std::io::stdout().lock();
    out.write_all(bytes)
        .and_then(|()| out.flush())
        .map_err(|e| DistError::Io {
            path: "stdout".to_string(),
            message: e.to_string(),
        })
}

/// `render mdbook [supports <renderer>]`: the preprocessor protocol of mdBook.
pub fn mdbook(cli: &Cli, args: &[String]) -> Result<ExitCode, DistError> {
    match args {
        [] => {}
        [word, renderer] if word == "supports" => {
            return Ok(if renderer == "html" {
                ExitCode::Ok
            } else {
                ExitCode::Error
            });
        }
        _ => {
            return Err(DistError::Usage(
                "render mdbook takes nothing, or `supports <renderer>`".to_string(),
            ));
        }
    }
    let input: Value = serde_json::from_slice(&read_stdin()?)
        .map_err(|e| DistError::Usage(format!("mdBook input is not JSON: {e}")))?;
    let [context, book] = <[Value; 2]>::try_from(input.as_array().cloned().unwrap_or_default())
        .map_err(|_| DistError::Usage("mdBook input is not a [context, book] pair".to_string()))?;
    let mut book = book;
    let src: PathBuf = Path::new(context["root"].as_str().unwrap_or("."))
        .join(context["config"]["book"]["src"].as_str().unwrap_or("src"));

    let mut lazy = Lazy::new(cli);
    let mut any = false;
    let mut failures = Vec::new();
    for key in ["sections", "items"] {
        if let Some(items) = book.get_mut(key).and_then(Value::as_array_mut) {
            chapters(items, &mut |chapter| {
                let Some(content) = chapter["content"].as_str() else {
                    return;
                };
                let Some(path) = chapter["path"].as_str() else {
                    return;
                };
                if find_blocks(content).is_empty() {
                    return;
                }
                let depth = path.matches(['/', '\\']).count();
                let prefix = format!("{}{ASSETS_DIR}/", "../".repeat(depth));
                let done = replace_doc_blocks(content, None, |spec| lazy.view(spec));
                any = true;
                failures.extend(done.failures.iter().cloned());
                chapter["content"] =
                    Value::String(format!("{}\n{}", head_tags(Some(&prefix)), done.text));
            });
        }
    }
    if any {
        write_assets(&src.join(ASSETS_DIR))?;
    }
    warn(&failures);
    let out = serde_json::to_string(&book).expect("serialise book");
    write_stdout(out.as_bytes())?;
    Ok(ExitCode::Ok)
}

/// Calls `f` on every chapter of `items`, depth first.
fn chapters(items: &mut [Value], f: &mut dyn FnMut(&mut Value)) {
    for item in items {
        if let Some(chapter) = item.get_mut("Chapter") {
            f(chapter);
            if let Some(sub) = chapter.get_mut("sub_items").and_then(Value::as_array_mut) {
                chapters(sub, f);
            }
        }
    }
}

/// `render filter [--assets inline|<dir>] [--strict]`: Markdown on stdin, Markdown on stdout.
pub fn filter(cli: &Cli, assets: &str, strict: bool) -> Result<ExitCode, DistError> {
    let bytes = read_stdin()?;
    // Input that is not text, or holds no query block, goes back byte for byte.
    let Ok(text) = std::str::from_utf8(&bytes) else {
        write_stdout(&bytes)?;
        return Ok(ExitCode::Ok);
    };
    if find_blocks(text).is_empty() {
        write_stdout(&bytes)?;
        return Ok(ExitCode::Ok);
    }
    let head = if assets == "inline" {
        head_tags(None)
    } else {
        write_assets(Path::new(assets))?;
        head_tags(Some(assets))
    };
    let mut lazy = Lazy::new(cli);
    let done: DocBlocks = replace_doc_blocks(text, Some(&head), |spec| lazy.view(spec));
    warn(&done.failures);
    write_stdout(done.text.as_bytes())?;
    Ok(match (strict, done.failures.first()) {
        (true, Some(f)) => exit_of(f.code),
        _ => ExitCode::Ok,
    })
}
