//! `lineage`: what a type is and where it goes (FEAT-101).
//!
//! The facade resolves the text as `callers` does, fills the code kinds from the registry as
//! `dead_code` does, and runs the query crate's walk in one snapshot.

use crate::{DistError, Katalogr, Options, Outcome, Reply, on_target, registry_code_kinds};
use katalogr_query::{FOLD_FROM, LineageOptions, LineageReport};

impl Katalogr {
    /// Resolves `text` as `callers` does, so ambiguous and no match pass through, a contract
    /// name is a target, and `at` is honoured. Fills `code_kinds`, sets `projects`, and copies
    /// `partial` and `folded` to the `Reply`. `opts.expand` removes folding and sets
    /// `per_group` to `limit`.
    pub fn lineage(
        &self,
        text: &str,
        mut lopts: LineageOptions,
        opts: &Options,
    ) -> Result<Reply<LineageReport>, DistError> {
        let _extra = self.extra_scope();
        lopts.code_kinds = registry_code_kinds();
        lopts.fold_from = (!opts.expand).then_some(FOLD_FROM);
        if opts.expand {
            lopts.per_group = lopts.limit;
        }
        let project = self
            .project
            .base()
            .root()
            .file_name()
            .unwrap_or("")
            .to_string();
        let mut folds = Vec::new();
        let mut reply = self.query(opts, |tx| {
            on_target(tx, text, |t| {
                let a = katalogr_query::lineage(tx, t, &lopts)?;
                folds = a.folded;
                let mut report = a.report;
                report.projects = vec![project.clone()];
                Ok(report)
            })
        })?;
        if let Outcome::Found(r) = &reply.outcome {
            reply.partial = r.partial.clone();
        }
        reply.folded = folds;
        Ok(reply)
    }
}
