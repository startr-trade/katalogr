# Impact

**Question answered:** if I change this, what else does it reach, and which tests cover it?

`impact` follows links outward from a starting point and groups what it reaches by link kind and
confidence, then lists the tests that cover the reached code.

```text
katalogr impact [SYMBOL] [--file PATH | --range BASE..HEAD | --since REV | --item CODE]
                [--depth N] [--test-depth N] [--limit N]
```

Give at most one starting point: a **symbol** or contract name, `--file`, `--range`, `--since` or
`--item`. With none, `impact` uses the changes since the merge-base with the default branch,
including uncommitted ones.

| Flag | Meaning |
|---|---|
| `--file <PATH>` | A file: `<repo>:<path>`, or a path relative to the marked repository |
| `--range <BASE..HEAD>` | A change range; the head must be a position the index holds |
| `--since <REV>` | Changes since a revision or clean position, up to `--at` (default: this checkout) |
| `--item <CODE>` | A board item code, for example `FEAT-7` |
| `--depth <N>` | How many hops to follow (default 3) |
| `--test-depth <N>` | How many hops the tests section follows (default 3) |
| `--limit <N>` | Most nodes in the groups, and most tests (default 500) |

## From a symbol

```text
$ katalogr impact add
impact of ledger:src/totals.rs#fn:add (symbol); project ledger
calls [extracted] 2
  ledger:src/lib.rs#fn:record  depth 1  1.0000
  ledger:src/totals.rs#fn:report  depth 1  1.0000
tests:
  case  ledger:src/lib.rs#mod:tests/fn:records_a_payment  depth 2  extracted  1.0000  through ledger:src/lib.rs#fn:record
```

The first line says what the starting point resolved to (a symbol, a file, a change, an item) and
the project. Then one group per link kind and confidence, such as `calls [extracted] 2`; the number
is how many nodes the group holds. Each node shows its depth and a weight (1.0000 for links read
straight from the source). The `tests:` section lists the tests over
the reached code, with the depth at which each was found.

A symbol nothing reaches prints only the first line. That is an answer, not an error: nothing
indexed depends on it.

## From a file or a change

```text
$ katalogr impact --file src/totals.rs
impact of ledger:src/totals.rs (file), ledger:src/totals.rs#fn:add (file), ledger:src/totals.rs#fn:report (file); project ledger
calls [extracted] 1
  ledger:src/lib.rs#fn:record  depth 1  1.0000
links-to [extracted] 1
  ledger:docs/overview.md#section:ledger-overview  depth 1  1.0000
```

A file stands for everything declared in it. Here a Markdown note that links to the file is part of
the impact, which is how a code change leads you to the documents that describe it. For documents
only, see [`docs-affected`](change-and-history.md#docs-affected).

`katalogr impact` with no argument, and `--since <REV>`, work from the lines that changed. They
start from what changed. `--range <BASE..HEAD>` is the same for an explicit range of commits.

```text
$ katalogr impact --since HEAD~1
impact of ledger:src/lib.rs#mod:tests/fn:records_a_payment (symbol); project ledger
tests:
  case  ledger:src/lib.rs#mod:tests/fn:records_a_payment  depth 0  extracted  1.0000
```

## From a board item

`--item <CODE>` starts at the item's specs and at the code its `Refs:` commits and annotations
reach. See [Connecting to kanbanr](../guide/kanbanr.md#4-ask-from-the-item).

## Reading the result

- **Depth** is the number of hops from the starting point. Depth 0 is the starting point itself
  (a changed test is its own impact).
- **Confidence** decides how much to trust a group. An `[inferred]` group is worth checking by
  hand.
- **Truncation.** When `--limit` cuts the answer it says `partial`. Raise the limit, or lower
  `--depth`.
- **Across projects.** With peers named in the project, `impact` also follows contracts into the
  peer projects. `--no-peers` limits it to this project. See [Workspaces](workspaces.md).

Exit codes: 0 for an answer (even an empty one); 3 if the symbol is ambiguous; 6 if `--at` is not
held by any context.

See also: the impact view in [Views and diagrams](views-and-diagrams.md) and the web view in
[Web view and exports](web-and-exports.md).

---

What changed in each version is in the [release notes](../releases/index.md).
