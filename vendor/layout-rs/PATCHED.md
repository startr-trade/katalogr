# Patched copy of layout-rs 0.1.3

Upstream: crates.io `layout-rs` 0.1.3 (MIT), the latest release when copied. Tracked as kanbanr FEAT-125.

## Change

Two edits, no other differences from upstream:

1. `src/adt/dag.rs`, `DAG::is_reachable_inner`: the node stays marked visited when the search backs out (the `visited[from.idx] = false` pop is removed). Reachability is now O(V+E) instead of enumerating every path.
2. `src/topo/layout.rs`, `to_valid_dag`: `self.dag.verify()` runs once after all edges are added instead of after each edge.

## Why

On a dense DAG (a 10k-symbol structure view) `to_svg` ran 18 CPU minutes, 99.8% in `is_reachable_inner`: it is called once per edge to reverse back edges, and `verify` calls it again for every edge, after every added edge. Layout output is unchanged for graphs that already worked.

## Upstream

This should be sent upstream as a fix; drop this copy and the `[patch.crates-io]` entry in the workspace `Cargo.toml` once a release contains it.
