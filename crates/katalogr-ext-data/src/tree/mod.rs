//! Documents as generic trees with spans: JSON, YAML and XML (FEAT-009 section 3.2).
//!
//! The parsers never fail as a whole: a syntax error is recorded with its span and the tree
//! built before it is kept. They fetch nothing, expand no alias and process no DTD.

mod json;
mod xml;
mod yaml;

use katalogr_model::{ExtractError, Span};

pub use json::parse_json;
pub use xml::parse_xml;
pub use yaml::parse_yaml;

/// The deepest nesting of containers a tree holds.
pub const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scalar {
    Str(String),
    /// A number as written.
    Num(String),
    Bool(bool),
    Null,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Scalar(Scalar),
    /// Entries in document order; a repeated key is kept as a repeated entry.
    Map(Vec<Entry>),
    Seq(Vec<Node>),
    /// A YAML alias, kept unexpanded as its anchor name.
    Alias(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub key_span: Span,
    pub value: Node,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub span: Span,
    pub value: Value,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub docs: Vec<Node>,
    pub errors: Vec<ExtractError>,
}

impl Node {
    /// The value of the first entry named `key` when this is a map.
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.entries()
            .iter()
            .find(|e| e.key == key)
            .map(|e| &e.value)
    }

    /// The entries when this is a map; none otherwise.
    pub fn entries(&self) -> &[Entry] {
        match &self.value {
            Value::Map(e) => e,
            _ => &[],
        }
    }

    /// The elements when this is a sequence; none otherwise.
    pub fn items(&self) -> &[Node] {
        match &self.value {
            Value::Seq(s) => s,
            _ => &[],
        }
    }

    /// The string form of a scalar (`Str` as is, `Num` as written, `true`/`false`, `null`);
    /// None otherwise.
    pub fn as_text(&self) -> Option<&str> {
        match &self.value {
            Value::Scalar(Scalar::Str(s) | Scalar::Num(s)) => Some(s),
            Value::Scalar(Scalar::Bool(true)) => Some("true"),
            Value::Scalar(Scalar::Bool(false)) => Some("false"),
            Value::Scalar(Scalar::Null) => Some("null"),
            _ => None,
        }
    }

    /// True when this is a map.
    pub fn is_map(&self) -> bool {
        matches!(self.value, Value::Map(_))
    }
}

pub fn parse_error(span: Option<Span>, message: impl Into<String>) -> ExtractError {
    ExtractError::Parse {
        span,
        message: message.into(),
    }
}
