//! `deps` (FEAT-007 R-8): the package versions a dependency inventory lists, in one read.

use crate::{DistError, Katalogr, Options, Outcome, Reply};
pub use katalogr_query::DepsOptions;
use katalogr_query::{DepHit, Partial, cap};

/// The version count `deps` returns when the caller gives no limit.
pub const DEFAULT_DEPS_LIMIT: usize = 1000;
/// The users listed per package when the caller gives no limit.
pub const DEFAULT_USED_BY_LIMIT: usize = 20;

impl Katalogr {
    /// One read; a cut adds `Partial::new(cap::LIMIT, limit, shown, Some(total))`.
    pub fn deps(
        &self,
        dopts: &DepsOptions,
        opts: &Options,
    ) -> Result<Reply<Vec<DepHit>>, DistError> {
        let mut cut = None;
        let mut reply = self.query(opts, |tx| {
            let (hits, total) = katalogr_query::deps(tx, dopts)?;
            if total > hits.len() as u64 {
                cut = Some(Partial::new(
                    cap::LIMIT,
                    dopts.limit as u64,
                    hits.len() as u64,
                    Some(total),
                ));
            }
            Ok(Outcome::Found(hits))
        })?;
        reply.partial.extend(cut);
        Ok(reply)
    }
}
