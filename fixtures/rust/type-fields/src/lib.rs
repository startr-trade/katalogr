//! Fields of code types.
use std::collections::HashMap;

pub mod model {
    pub struct Line {
        pub sku: String,
    }
}

use model::Line;

/// An order.
pub struct Order {
    /// The identifier.
    pub id: String,
    pub note: Option<String>,
    pub lines: Vec<Line>,
    pub blob: Vec<u8>,
    pub tags: HashMap<String, String>,
    pub first: crate::model::Line,
    pub count: u32,
    pub ratio: f64,
    pub paid: bool,
    pub when: chrono::DateTime<chrono::Utc>,
    pub local: Option<Box<Order>>,
}

pub struct Pair(pub i64, Line);

pub union Raw {
    pub word: u32,
    pub real: f32,
}

pub enum Event {
    Placed { id: String, line: Line },
    Moved(String, Option<Line>),
    Done,
}
