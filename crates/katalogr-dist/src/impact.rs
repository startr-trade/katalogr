//! `impact`: what a symbol, file, change range or board item reaches (FEAT-028).
//!
//! The facade resolves the input to seeds in one snapshot and runs the query crate's walk. Git
//! runs before the snapshot, so a commit landing between them can only make the answer inexact.

use crate::{DistError, Katalogr, Options, Outcome, Reply, unresolved};
use katalogr_model::{AnyName, RelPath, parse_name};
use katalogr_query::{
    ImpactOptions, ImpactReport, Resolution, Seed, seeds_for_changes, seeds_for_file,
    seeds_for_item, seeds_for_symbol,
};
use std::time::Instant;

/// What an `impact` question starts from.
pub enum ImpactInput<'a> {
    /// A symbol or contract, as `callers` resolves its text.
    Symbol(&'a str),
    /// A canonical artifact name (text with `:`), else a path relative to the marked repo.
    File(&'a str),
    /// A board item code.
    Item(&'a str),
    /// `base..head`, both resolved to commits.
    Range(&'a str),
    /// Changes since a revision or clean position; `None`: the merge-base with the default
    /// branch, up to the working tree.
    Since(Option<&'a str>),
}

/// How the input becomes seeds inside the snapshot.
enum Plan {
    Symbol(String),
    File { repo: String, path: RelPath },
    Item(String),
    Changes(crate::changes::ChangeRange),
}

impl Katalogr {
    /// Resolves the seeds and reads one snapshot. Ambiguous and no match pass through as for
    /// `callers`. A range's head that no context holds is `PositionUnavailable`; `impact`
    /// never queues or indexes to reach a position.
    pub fn impact(
        &self,
        input: &ImpactInput<'_>,
        iopts: &ImpactOptions,
        opts: &Options,
    ) -> Result<Reply<ImpactReport>, DistError> {
        let _extra = self.extra_scope();
        let mut opts = opts.clone();
        let mut base: Option<String> = None;
        let plan = match input {
            ImpactInput::Symbol(t) => Plan::Symbol((*t).to_string()),
            ImpactInput::Item(c) => Plan::Item((*c).to_string()),
            ImpactInput::File(t) => {
                let (repo, path) = if t.contains(':') {
                    match parse_name(t) {
                        Ok(AnyName::Artifact(a)) => (a.repo, a.path),
                        _ => {
                            return Err(DistError::usage(format!(
                                "--file {t}: not an artifact name (<repo>:<path>)"
                            )));
                        }
                    }
                } else {
                    let (repo, _) = self.current()?;
                    let path = RelPath::new(t)
                        .map_err(|e| DistError::usage(format!("--file {t}: {e}")))?;
                    (repo.to_string(), path)
                };
                Plan::File { repo, path }
            }
            ImpactInput::Range(text) => {
                let (range, head) = self.range_changes(text)?;
                opts.at = Some(head);
                base = Some(range.from.clone());
                Plan::Changes(range)
            }
            ImpactInput::Since(since) => {
                let deadline = Instant::now() + opts.timeout()?;
                let range = self.change_range(*since, &opts, deadline)?;
                base = Some(range.from.clone());
                Plan::Changes(range)
            }
        };
        if let Some(b) = base {
            // A revision name can move while the positions stay: the commit names the answer.
            self.view_extra
                .lock()
                .expect("view_extra lock")
                .push(("base".to_string(), b));
        }
        let project = self
            .project
            .base()
            .root()
            .file_name()
            .unwrap_or("")
            .to_string();
        let mut reply = self.query(&opts, |tx| {
            let seeds: Vec<Seed> = match &plan {
                Plan::Symbol(text) => match katalogr_query::resolve(tx, text)? {
                    Resolution::One(t) => seeds_for_symbol(&t),
                    other => return Ok(unresolved(other, text)),
                },
                Plan::File { repo, path } => match seeds_for_file(tx, repo, path)? {
                    Some(s) => s,
                    None => {
                        return Ok(Outcome::NoMatch(format!(
                            "no indexed file {repo}:{}",
                            path.as_str()
                        )));
                    }
                },
                Plan::Item(code) => match seeds_for_item(tx, code)? {
                    Some(s) => s,
                    None => return Ok(Outcome::NoMatch(format!("no item {code:?} in the index"))),
                },
                Plan::Changes(r) => seeds_for_changes(tx, &r.repo, &r.changes)?,
            };
            let mut report = katalogr_query::impact(tx, &seeds, iopts)?;
            report.projects = vec![project.clone()];
            Ok(Outcome::Found(report))
        })?;
        if let Outcome::Found(r) = &reply.outcome {
            reply.partial = r.partial.clone();
        }
        Ok(reply)
    }
}
