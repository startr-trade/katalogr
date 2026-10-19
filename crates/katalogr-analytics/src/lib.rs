//! Structure insights over one graph snapshot: hubs, communities, cycles, dead code and rationale (FEAT-030).

mod cycles;
#[allow(dead_code)]
mod graph;
mod hints;
pub mod options;
pub mod result;
mod scc;
mod schema;
#[cfg(test)]
#[allow(dead_code)]
mod testkit;

pub use cycles::cycles;
pub use options::*;
pub use result::*;
pub use schema::schemas;
