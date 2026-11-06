//! Reports at each merge (FEAT-095 R-16). A git hook only writes a small request file; a later
//! `index` or `diff --requested` builds the report, so a merge never waits for one.

use crate::diff::{DIFF_TIMEOUT_DEFAULT, DiffRender, DiffRequest};
use crate::{DistError, Katalogr, Options};
use katalogr_store::ProjectDir;
use std::path::PathBuf;

/// Requests kept at most; past it the oldest by name go.
pub const MAX_REQUESTS: usize = 32;

/// What `diff_requested` did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestOutcome {
    pub built: u64,
    pub failed: u64,
    pub warnings: Vec<String>,
}

fn dir_of(project: &ProjectDir) -> PathBuf {
    project
        .cache_dir()
        .join("diff-requests")
        .into_std_path_buf()
}

/// Best effort, one small file: `<repo>-<a40>-<b40>.req`, empty. Processes nothing. Keeps the
/// newest [`MAX_REQUESTS`] by name.
#[doc(hidden)]
pub fn queue_diff_request(
    project: &ProjectDir,
    repo: &str,
    a: &str,
    b: &str,
) -> Result<(), DistError> {
    let dir = dir_of(project);
    std::fs::create_dir_all(&dir).map_err(|e| DistError::io(dir.display(), e))?;
    let file = dir.join(format!("{repo}-{a}-{b}.req"));
    std::fs::write(&file, b"").map_err(|e| DistError::io(file.display(), e))?;
    let mut names = request_files(&dir)?;
    names.sort();
    while names.len() > MAX_REQUESTS {
        let oldest = names.remove(0);
        let _ = std::fs::remove_file(dir.join(oldest));
    }
    Ok(())
}

fn request_files(dir: &std::path::Path) -> Result<Vec<String>, DistError> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(DistError::io(dir.display(), e)),
    };
    Ok(rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".req"))
        .collect())
}

/// `(repo, a, b)` of a request file name. The repo may hold `-`; the two commits do not.
fn parse(name: &str) -> Option<(String, String, String)> {
    let stem = name.strip_suffix(".req")?;
    let (rest, b) = stem.rsplit_once('-')?;
    let (repo, a) = rest.rsplit_once('-')?;
    (!repo.is_empty() && a.len() == 40 && b.len() == 40)
        .then(|| (repo.to_string(), a.to_string(), b.to_string()))
}

impl Katalogr {
    /// Whether a merge queues a report (project setting `diff.on_merge`, default true).
    pub(crate) fn diff_on_merge(&self) -> bool {
        self.project
            .config()
            .settings
            .get("diff.on_merge")
            .and_then(serde_yaml_ng::Value::as_bool)
            .unwrap_or(true)
    }

    /// Builds all three formats of each queued request, in name order. One that fails stays
    /// queued.
    pub fn diff_requested(&self, o: &Options) -> Result<RequestOutcome, DistError> {
        let dir = dir_of(&self.project);
        let mut names = request_files(&dir)?;
        names.sort();
        let mut out = RequestOutcome::default();
        if names.is_empty() {
            return Ok(out);
        }
        if !self.answers_enabled() {
            out.warnings.push(format!(
                "stored answers are off, so {} requested report(s) stay queued",
                names.len()
            ));
            return Ok(out);
        }
        let mut o = o.clone();
        o.timeout.get_or_insert(DIFF_TIMEOUT_DEFAULT as f64);
        for name in names {
            let Some((repo, a, b)) = parse(&name) else {
                out.failed += 1;
                out.warnings
                    .push(format!("request {name} is not understood"));
                continue;
            };
            let req = DiffRequest {
                a: a.clone(),
                b: b.clone(),
                repo: Some(repo),
                limit: 200,
            };
            let built = self.diff_report(&req, &o).and_then(|_| {
                self.diff_page(&req, DiffRender::Markdown, &o)?;
                self.diff_page(&req, DiffRender::Html, &o)
            });
            match built {
                Ok(_) => {
                    let _ = std::fs::remove_file(dir.join(&name));
                    out.built += 1;
                }
                Err(e) => {
                    out.failed += 1;
                    out.warnings.push(format!(
                        "report {}..{} failed and stays queued: {e}",
                        &a[..12],
                        &b[..12]
                    ));
                }
            }
        }
        Ok(out)
    }
}
