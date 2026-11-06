//! The typed view manifest: the one list of views, read from `views.json`. Rust reads it for
//! tests; the page's scripts read the same text to draw.

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub wait_seconds: u32,
    pub views: Vec<ViewRow>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ViewRow {
    pub id: String,
    pub title: String,
    pub link: bool,
    pub nav: bool,
    pub kind: String,
    pub needs: Option<String>,
    pub absent: Option<String>,
    #[serde(default)]
    pub panels: Vec<Panel>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Panel {
    pub id: String,
    pub title: String,
    pub needs: Option<String>,
    pub absent: Option<String>,
    pub op: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    pub rows: Option<String>,
    #[serde(default)]
    pub columns: Vec<Column>,
    pub empty: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Column {
    pub title: String,
    pub path: String,
    pub join: Option<String>,
    #[serde(default)]
    pub symbol_link: bool,
}

/// Parses `VIEWS_JSON`. Panics on a bad file: a test proves it parses.
pub fn manifest() -> Manifest {
    serde_json::from_str(crate::VIEWS_JSON).expect("views.json parses")
}
