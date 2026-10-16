//! Extension registry: maps a path to the extractor that handles it.

use katalogr_model::{Extractor, RelPath};

/// Extractors in registration order; the first one that handles a path wins.
#[derive(Default)]
pub struct ExtensionRegistry {
    extractors: Vec<Box<dyn Extractor>>,
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, extractor: Box<dyn Extractor>) {
        self.extractors.push(extractor);
    }

    /// First registered extractor whose `handles` is true for `path`.
    pub fn for_path(&self, path: &RelPath) -> Option<&dyn Extractor> {
        self.extractors
            .iter()
            .find(|e| e.handles(path))
            .map(|e| e.as_ref())
    }

    /// Extension ids in registration order.
    pub fn ids(&self) -> Vec<&'static str> {
        self.extractors.iter().map(|e| e.id()).collect()
    }
}
