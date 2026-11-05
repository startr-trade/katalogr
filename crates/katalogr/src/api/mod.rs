//! The stateless HTTP/JSON API server (FEAT-049, ADR-0010): a thin wrapper over katalogr-dist,
//! built like `mcp/`. Request parsing and answers are in [`http`], authorisation in [`guard`].
//! The accept loop, the op table and `run` follow in the next tasks.

// The pieces are wired into `run` by a later task of the item.
#![allow(dead_code)]

pub mod guard;
pub mod http;
