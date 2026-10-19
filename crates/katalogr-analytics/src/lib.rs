//! Structure insights over one graph snapshot: hubs, communities, cycles, dead code and rationale (FEAT-030).

mod communities;
mod cycles;
mod dead;
mod graph;
mod hints;
mod hubs;
mod louvain;
pub mod metrics;
pub mod options;
mod rationale;
pub mod result;
mod scc;
mod schema;
#[cfg(test)]
#[allow(dead_code)]
mod testkit;

pub use communities::communities;
#[doc(hidden)]
pub use communities::communities_with_stats;
pub use cycles::cycles;
pub use dead::dead_code;
pub use hubs::hubs;
#[doc(hidden)]
pub use louvain::LouvainStats;
pub use options::*;
pub use rationale::rationale;
pub use result::*;
pub use schema::schemas;
