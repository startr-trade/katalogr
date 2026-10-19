// WHY: a leading comment explains why the function exists.
export function leading() {
  // NOTE: a body comment belongs to the function that holds it.
  return 1;
}

/**
 * NOTE(perf): a block comment continues
 * over two lines.
 */
export function documented() {}

// Note that prose is not a marker.
export function plain() {}

// RATIONALE: nothing follows, so this one belongs to the file.
