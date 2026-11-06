//! `docs-affected`: which documents are linked to code that changed (FEAT-086).

use crate::changes::ChangeRange;
use crate::{DistError, Katalogr, Options, Outcome, Reply};
use katalogr_query::{DOC_LINK_KINDS, DocImpact, DocImpactInput};
use std::time::Instant;

/// Default number of documents reported.
pub const DEFAULT_DOCS_LIMIT: usize = 200;

/// What a `docs-affected` question adds to the read options.
#[derive(Debug, Clone)]
pub struct DocsAffectedParams {
    /// Link kinds that connect a document to code; empty means the `docs.link_kinds` project
    /// setting, else [`DOC_LINK_KINDS`].
    pub kinds: Vec<String>,
    /// Most documents to report.
    pub limit: usize,
}

impl Default for DocsAffectedParams {
    fn default() -> Self {
        DocsAffectedParams {
            kinds: Vec::new(),
            limit: DEFAULT_DOCS_LIMIT,
        }
    }
}

impl Katalogr {
    /// The `docs.link_kinds` setting, else [`DOC_LINK_KINDS`].
    fn doc_link_kinds(&self) -> Vec<String> {
        self.project
            .config()
            .settings
            .get("docs.link_kinds")
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DOC_LINK_KINDS.iter().map(|k| k.to_string()).collect())
    }

    /// Documents linked to code that changed since `since` (a revision or a clean position;
    /// default the merge-base of `HEAD` and the default branch) up to `opts.at` (default this
    /// checkout's position, so uncommitted changes count). A dirty `since` is a usage error,
    /// since old uncommitted content cannot be diffed.
    pub fn docs_affected(
        &self,
        since: Option<&str>,
        params: &DocsAffectedParams,
        opts: &Options,
    ) -> Result<Reply<DocImpact>, DistError> {
        let deadline = Instant::now() + opts.timeout()?;
        let ChangeRange { repo, changes, .. } = self.change_range(since, opts, deadline)?;
        let kinds = if params.kinds.is_empty() {
            self.doc_link_kinds()
        } else {
            params.kinds.clone()
        };
        let mut reply = self.query(opts, |tx| {
            Ok(Outcome::Found(katalogr_query::doc_impact(
                tx,
                &DocImpactInput {
                    repo: &repo,
                    changes: &changes,
                    kinds: &kinds,
                    limit: params.limit,
                },
            )?))
        })?;
        if let Outcome::Found(d) = &reply.outcome {
            reply.partial = d.partial.clone();
        }
        Ok(reply)
    }
}
