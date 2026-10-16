//! Composition root: the Katalogr facade with extensions registered (ADR-0011).

/// The katalogr facade. Extensions are registered here; the CLI and other
/// wrappers talk to the core only through this type.
pub struct Katalogr;

impl Katalogr {
    /// Creates a facade with no extensions registered yet.
    pub fn new() -> Self {
        Katalogr
    }
}

impl Default for Katalogr {
    fn default() -> Self {
        Self::new()
    }
}
