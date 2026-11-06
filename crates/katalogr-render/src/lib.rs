//! Diagram rendering for graph views (FEAT-032). A [`ViewGraph`] is written as Mermaid, DOT,
//! GraphML, Structurizr or SVG, and the Markdown and Pele modules embed the result in doc tools.
//!
//! This crate is a library but not a core crate: Mermaid and DOT are formats, which the core
//! never names. It reads nothing from the index; callers build the graph.

pub mod assets;
pub mod cytoscape;
pub mod dot;
pub mod graph;
pub mod graphml;
pub mod markdown;
pub mod mermaid;
pub mod query_block;
pub mod structurizr;
pub mod svg;

#[allow(unused_imports)] // empty until its task lands
pub use assets::*;
#[allow(unused_imports)] // empty until its task lands
pub use cytoscape::*;
pub use dot::*;
pub use graph::*;
#[allow(unused_imports)] // empty until its task lands
pub use graphml::*;
#[allow(unused_imports)] // empty until its task lands
pub use markdown::*;
pub use mermaid::*;
#[allow(unused_imports)] // empty until its task lands
pub use query_block::*;
#[allow(unused_imports)] // empty until its task lands
pub use structurizr::*;
#[allow(unused_imports)] // empty until its task lands
pub use svg::*;

use std::fmt;

/// Why a view could not be parsed or laid out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderError {
    /// A query block or view specification that does not parse.
    Spec(String),
    /// The layout step refused the graph.
    Layout(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::Spec(m) => write!(f, "bad view: {m}"),
            RenderError::Layout(m) => write!(f, "layout failed: {m}"),
        }
    }
}

impl std::error::Error for RenderError {}
