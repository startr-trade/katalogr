//! RATIONALE: this file exists to show a file-level marker.

// WHY: a leading comment explains why the function exists.
pub fn leading() {
    // NOTE: a body comment belongs to the function that holds it.
    let _x = 1;
}

/// NOTE(perf): a doc comment continues
/// over two lines.
pub fn documented() {}

// Note that prose is not a marker.
pub fn plain() {}
