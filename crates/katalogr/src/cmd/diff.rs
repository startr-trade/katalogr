//! `katalogr diff <A> <B> [--repo NAME] [--format markdown|html] [--limit N] [--out FILE | --out-dir DIR]`
//! (FEAT-095).

use std::path::Path;

use katalogr_dist::diff::{DIFF_TIMEOUT_DEFAULT, DiffRender, DiffReport, DiffRequest};
use katalogr_dist::{DistError, Options, Outcome, Reply};

use crate::cli::{Cli, DiffFormat};
use crate::cmd::options;
use crate::exit::ExitCode;
use crate::render;

pub struct Args<'a> {
    pub a: Option<&'a str>,
    pub b: Option<&'a str>,
    pub requested: bool,
    pub repo: Option<&'a str>,
    pub format: Option<DiffFormat>,
    pub limit: usize,
    pub out: Option<&'a Path>,
    pub out_dir: Option<&'a Path>,
}

/// Write `text` to `path` by a temp file beside it and a rename. The temp file is removed when
/// either step fails.
fn write_atomic(path: &Path, text: &str) -> Result<(), DistError> {
    let name = path
        .file_name()
        .ok_or_else(|| DistError::Usage(format!("{}: not a file name", path.display())))?
        .to_string_lossy()
        .into_owned();
    let tmp = path.with_file_name(format!(".{name}.tmp{}", std::process::id()));
    let result = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(DistError::Usage(format!("{}: {e}", path.display())));
    }
    Ok(())
}

/// A path a report may be written to: one git would not track.
fn check_target(path: &Path) -> Result<(), DistError> {
    if katalogr_dist::output_outside_git(path)? {
        Ok(())
    } else {
        Err(DistError::Usage(format!(
            "{}: git would track this path, and a report holds names and paths from the index; choose a folder outside the work tree or one git ignores",
            path.display()
        )))
    }
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

/// The envelope line of a report reply, with its warnings sent to standard error.
fn envelope(reply: Reply<DiffReport>) -> (String, ExitCode) {
    let a = render::json::plain(reply, |_| false);
    for w in &a.warnings {
        eprintln!("katalogr: warning: {w}");
    }
    (a.stdout.clone().unwrap_or_default(), a.exit)
}

fn page_text(
    k: &katalogr_dist::Katalogr,
    r: &DiffRequest,
    f: DiffRender,
    o: &Options,
) -> Result<String, DistError> {
    let reply = k.diff_page(r, f, o)?;
    for w in &reply.report.warnings {
        eprintln!("katalogr: warning: {w}");
    }
    match reply.outcome {
        Outcome::Found(p) => Ok(p.text),
        _ => Err(DistError::Usage("no report could be made".into())),
    }
}

pub fn run(cli: &Cli, args: &Args) -> Result<ExitCode, DistError> {
    let k = crate::cmd::resolve(cli)?;
    let mut o = options(cli);
    o.timeout = Some(cli.timeout.unwrap_or(DIFF_TIMEOUT_DEFAULT as f64));
    if args.requested {
        let o = k.diff_requested(&o)?;
        for w in &o.warnings {
            eprintln!("katalogr: warning: {w}");
        }
        if cli.json {
            println!(
                "{}",
                serde_json::json!({ "built": o.built, "failed": o.failed })
            );
        } else {
            println!("built {} report(s), {} failed", o.built, o.failed);
        }
        return Ok(if o.failed > 0 {
            ExitCode::Error
        } else {
            ExitCode::Ok
        });
    }
    let (Some(a), Some(b)) = (args.a, args.b) else {
        return Err(DistError::Usage(
            "name two revisions, or use --requested".into(),
        ));
    };
    let req = DiffRequest {
        a: a.to_string(),
        b: b.to_string(),
        repo: args.repo.map(str::to_string),
        limit: args.limit,
    };
    let render_of = |f: Option<DiffFormat>| match f {
        Some(DiffFormat::Html) => DiffRender::Html,
        _ => DiffRender::Markdown,
    };

    if let Some(dir) = args.out_dir {
        // A cheap refusal before the work; each file is checked again under its own name.
        check_target(&dir.join("diff.json"))?;
        let report = k.diff_report(&req, &o)?;
        let (a12, b12) = match &report.outcome {
            Outcome::Found(d) => (
                short(&d.a.commit).to_string(),
                short(&d.b.commit).to_string(),
            ),
            _ => return Err(DistError::Usage("no report could be made".into())),
        };
        let md = page_text(&k, &req, DiffRender::Markdown, &o)?;
        let html = page_text(&k, &req, DiffRender::Html, &o)?;
        std::fs::create_dir_all(dir)
            .map_err(|e| DistError::Usage(format!("{}: {e}", dir.display())))?;
        let (json, code) = envelope(report);
        let stem = format!("diff-{a12}-{b12}");
        for ext in ["json", "md", "html"] {
            check_target(&dir.join(format!("{stem}.{ext}")))?;
        }
        write_atomic(&dir.join(format!("{stem}.json")), &format!("{json}\n"))?;
        write_atomic(&dir.join(format!("{stem}.md")), &md)?;
        write_atomic(&dir.join(format!("{stem}.html")), &html)?;
        println!(
            "wrote {stem}.json, {stem}.md and {stem}.html to {}",
            dir.display()
        );
        return Ok(code);
    }

    if let Some(file) = args.out {
        check_target(file)?;
        let text = if cli.json {
            let (json, _) = envelope(k.diff_report(&req, &o)?);
            format!("{json}\n")
        } else {
            page_text(&k, &req, render_of(args.format), &o)?
        };
        write_atomic(file, &text)?;
        return Ok(ExitCode::Ok);
    }

    if cli.json {
        let (json, code) = envelope(k.diff_report(&req, &o)?);
        println!("{json}");
        return Ok(code);
    }
    print!("{}", page_text(&k, &req, render_of(args.format), &o)?);
    Ok(ExitCode::Ok)
}
