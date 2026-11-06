//! `katalogr render <op> [args] [--format F]`, plus the doc-tool modes `mdbook` and `filter`.

use clap::ValueEnum;
use katalogr_dist::query::{Partial, cap};
use katalogr_dist::render::{
    ViewGraph, ViewSpec, to_dot, to_graphml, to_mermaid, to_structurizr, to_svg,
};
use katalogr_dist::{DistError, Outcome, Reply, ViewBytes};

use crate::cli::{Cli, RenderAction, RenderFormat};
use crate::cmd::{options, render_doc, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(
    cli: &Cli,
    action: Option<&RenderAction>,
    format: RenderFormat,
    words: &[String],
) -> Result<ExitCode, DistError> {
    match action {
        Some(RenderAction::Mdbook { args }) => return render_doc::mdbook(cli, args),
        Some(RenderAction::Filter { assets, strict }) => {
            return render_doc::filter(cli, assets, *strict);
        }
        None => {}
    }
    if words.is_empty() {
        return Err(DistError::Usage(
            "render needs a view: structure, contracts, callers <symbol>, path <from> <to> or cycles"
                .to_string(),
        ));
    }
    let (format, words) = take_format(format, words)?;
    let spec = ViewSpec::parse(&line_of(&words)).map_err(|e| DistError::Usage(e.to_string()))?;
    // The graph comes from the stored view when there is one; a format is never a new
    // computation (FEAT-088).
    let reply = resolve(cli)?.view(&spec, &options(cli))?;
    render::view(cli, graph_reply(&spec, reply)?, |g| draw(g, format))
}

/// The reply of `Katalogr::view` as a reply holding its graph. A cut view is marked partial
/// whether it was computed or read back.
fn graph_reply(spec: &ViewSpec, reply: Reply<ViewBytes>) -> Result<Reply<ViewGraph>, DistError> {
    let Reply {
        outcome,
        report,
        diagnostic,
        mut partial,
        folded,
        workspace,
    } = reply;
    let outcome = match outcome {
        Outcome::Found(vb) => {
            let answer: serde_json::Value = serde_json::from_slice(&vb.body)
                .map_err(|e| DistError::Usage(format!("stored view: {e}")))?;
            let g: ViewGraph = serde_json::from_value(answer["graph"].clone())
                .map_err(|e| DistError::Usage(format!("stored view: {e}")))?;
            if partial.is_empty() && g.total > g.shown {
                let cap = ["max-nodes", "limit"]
                    .iter()
                    .find_map(|k| spec.args.get(*k))
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(g.shown);
                partial.push(Partial::new(cap::LIMIT, cap, g.shown, Some(g.total)));
            }
            Outcome::Found(g)
        }
        Outcome::Ambiguous(c) => Outcome::Ambiguous(c),
        Outcome::NoMatch(m) => Outcome::NoMatch(m),
        Outcome::StaleFile(m) => Outcome::StaleFile(m),
        Outcome::PositionUnavailable { wanted, available } => {
            Outcome::PositionUnavailable { wanted, available }
        }
    };
    Ok(Reply {
        outcome,
        report,
        diagnostic,
        partial,
        folded,
        workspace,
    })
}

/// A `--format F` or `--format=F` among the view's words: the words after the op are taken
/// whole, so the flag is picked out here. The last one wins.
fn take_format(
    mut format: RenderFormat,
    words: &[String],
) -> Result<(RenderFormat, Vec<String>), DistError> {
    let parse = |v: &str| {
        RenderFormat::from_str(v, true).map_err(|_| {
            DistError::Usage(format!(
                "unknown --format `{v}`: use mermaid, dot, graphml, structurizr or svg"
            ))
        })
    };
    let mut rest = Vec::with_capacity(words.len());
    let mut it = words.iter();
    while let Some(w) = it.next() {
        if w == "--format" {
            let v = it
                .next()
                .ok_or_else(|| DistError::Usage("--format wants a value".to_string()))?;
            format = parse(v)?;
        } else if let Some(v) = w.strip_prefix("--format=") {
            format = parse(v)?;
        } else {
            rest.push(w.clone());
        }
    }
    Ok((format, rest))
}

/// The words back as one query line: a word with a space is quoted again.
pub fn line_of(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            if w.is_empty() || w.contains(char::is_whitespace) {
                format!("\"{w}\"")
            } else {
                w.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn draw(g: &ViewGraph, format: RenderFormat) -> Result<String, DistError> {
    Ok(match format {
        RenderFormat::Mermaid => to_mermaid(g),
        RenderFormat::Dot => to_dot(g),
        RenderFormat::Graphml => to_graphml(g),
        RenderFormat::Structurizr => to_structurizr(g),
        RenderFormat::Svg => to_svg(g).map_err(|e| DistError::Io {
            path: "svg".to_string(),
            message: e.to_string(),
        })?,
    })
}

/// Global flags that take a value.
const VALUE_FLAGS: &[&str] = &[
    "--kindex",
    "--at",
    "--project",
    "--consistency",
    "--timeout",
    "--budget",
];

/// `render` and `view` take their view's words whole, so a global flag written after the op (for example
/// `render structure --json`) would be read as one of the view's flags. This moves the global
/// flags after `render` to just before it; anything else is left as it was.
pub fn hoist_globals(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let text = |a: &std::ffi::OsString| a.to_str().map(str::to_string);
    // Find `render` as the first word that is not a global flag or its value.
    let mut at = None;
    let mut i = 1;
    while i < args.len() {
        let Some(a) = text(&args[i]) else { return args };
        if VALUE_FLAGS.contains(&a.as_str()) {
            i += 2;
        } else if a.starts_with('-') {
            i += 1;
        } else {
            if a == "render" || a == "view" {
                at = Some(i);
            }
            break;
        }
    }
    let Some(at) = at else { return args };
    if matches!(
        args.get(at + 1).and_then(text).as_deref(),
        Some("mdbook" | "filter")
    ) {
        return args;
    }
    let (mut front, mut moved, mut rest) = (args[..at].to_vec(), Vec::new(), Vec::new());
    let mut j = at + 1;
    while j < args.len() {
        let a = text(&args[j]).unwrap_or_default();
        let name = a.split('=').next().unwrap_or_default();
        if a == "--json" {
            moved.push(args[j].clone());
        } else if VALUE_FLAGS.contains(&name) {
            moved.push(args[j].clone());
            if !a.contains('=') && j + 1 < args.len() {
                j += 1;
                moved.push(args[j].clone());
            }
        } else {
            rest.push(args[j].clone());
        }
        j += 1;
    }
    front.extend(moved);
    front.push(args[at].clone());
    front.extend(rest);
    front
}

#[cfg(test)]
mod hoist {
    use super::hoist_globals;
    use std::ffi::OsString;

    fn v(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    #[test]
    fn moves_globals_before_render() {
        let got = hoist_globals(v(&[
            "katalogr",
            "render",
            "structure",
            "--json",
            "--level",
            "file",
            "--timeout",
            "3",
        ]));
        assert_eq!(
            got,
            v(&[
                "katalogr",
                "--json",
                "--timeout",
                "3",
                "render",
                "structure",
                "--level",
                "file"
            ])
        );
    }

    #[test]
    fn leaves_other_commands_and_doc_modes() {
        let a = v(&["katalogr", "find", "--json", "x"]);
        assert_eq!(hoist_globals(a.clone()), a);
        let b = v(&["katalogr", "render", "filter", "--strict"]);
        assert_eq!(hoist_globals(b.clone()), b);
    }
}
