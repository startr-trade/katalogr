# Connecting to kanbanr

kanbanr keeps a project's scope, requirements, tests and
decisions on a board. katalogr links that board to the code, so you can go both ways: from a line
of code to the item that asked for it, and from an item to the code, tests and documents it
reaches. This page covers katalogr's side of the connection. Everything here also works with no
kanbanr running: katalogr only reads files.

What katalogr reads:

1. The **board repository** itself, as one more repository of the project.
2. **Item references** in code, comments and documents.
3. **`Refs:` trailers** in git commit messages.

What katalogr gives back:

- `why`, `impact --item`, `docs-affected`, `rationale`, `tests` and `dangling`, which use those links.
- A versioned **contract**: a stable list of what this binary can answer, and views that carry
  an identity that kanbanr can compare.
- Generated **views in board documents**, from `katalogr-query` blocks.

## 1. Index the board

A kanbanr board is a git repository with one folder per project (`projects/<name>/`) that holds
the charter, one YAML file per item, specs and documents. Add it to the katalogr project as a
repository and record where it is on this machine. The example board here is a synthetic one for
`ledger`, with one item, `FEAT-7`, one requirement, `R-1`, and one goal, `G-1`.

In `kx/projects/ledger/config.yaml`, add a repo entry (see
[Getting started](getting-started.md#2-create-a-project-and-mark-the-repository)):

```yaml
repos:
- name: board
  include: ['**']
  exclude: ['.git/**']
- name: ledger
  …
```

```text
$ katalogr project add-root board /path/to/board
root of board recorded
$ katalogr index --full
queued 3 event(s); watermark 8, 0 pending
```

From the board, katalogr builds these symbols (shown as the `kind` column of any listing):

| Symbol | From | Canonical name (example) |
|---|---|---|
| `item` | `features/<Status>/FEAT-7.yaml` | `board:projects/ledger/features/Implementation/FEAT-7.yaml#item:FEAT-7` |
| `req` | each requirement of the item | `…#item:FEAT-7/req:R-1` |
| `task` | each task of a todo list | `…#item:FEAT-7/task:TL-001%2FT1` |
| `goal` | `charter.yaml` | `board:projects/ledger/charter.yaml#goal:G-1` |
| `adr` | `docs/decisions/*.md` | `…#adr:ADR-0001` |
| `section` | specs and documents | `…/FEAT-7.md#section:record-payments/section:design` |

Links among them: an item `serves` the goals it names, a spec `specifies` its item, and an ADR
`affects` the items it names. Because the item's status is part of its file path, moving an item to
another stage changes its canonical name. Use the plain name (`FEAT-7`) in commands.

```text
$ katalogr explain FEAT-7
item  board:projects/ledger/features/Implementation/FEAT-7.yaml#item:FEAT-7  projects/ledger/…/FEAT-7.yaml:1
outgoing (2):
  contains  board:projects/ledger/features/Implementation/FEAT-7.yaml#item:FEAT-7/req:R-1  [Extracted]
  serves  board:projects/ledger/charter.yaml#goal:G-1  [Extracted]
incoming (3):
  annotated-by  ledger:src/lib.rs#fn:record  [Extracted]
  refs-item  ledger:@history/19714ed1e468c5d45da0e94efabc536e8c7e076e  [Extracted]
  specifies  board:projects/ledger/…/features-spec/FEAT-7.md  [Extracted]
```

## 2. Reference items from code

Write the item code in a comment or a document, next to the code it explains. katalogr recognises
these forms, anywhere in the text of a comment or Markdown file:

| Written | Names |
|---|---|
| `FEAT-7` | the item |
| `FEAT-7 R-1` or `FEAT-7/R-1` | the item's requirement |
| `kanbanr:FEAT-7` | the item (the prefix makes it explicit) |
| `kanbanr:FEAT-7/TL-001/T3` | a task (it resolves to its item) |

A code is `[A-Z][A-Z0-9]*-<digits>`. Text inside a URL, or glued to other identifier characters
(`XFEAT-7`, `FEAT-7x`) is not a reference. In our `ledger`, the function carries:

```rust
/// Record a payment and return its id.
// FEAT-7: payments must never be lost
pub fn record(p: &Payment) -> u64 {
```

`why` takes a line and answers with the symbol that contains it and what explains it:

```text
$ katalogr why src/lib.rs:17
fn  ledger:src/lib.rs#fn:record  src/lib.rs:13
  caller  fn  ledger:src/lib.rs#mod:tests/fn:records_a_payment  src/lib.rs:28  [Extracted]  depth 1
  test  case  ledger:src/lib.rs#mod:tests/fn:records_a_payment  depth 1  extracted  1.0000
  commit  19714ed1e468  feat: first ledger
  annotated-by  FEAT-7  board:projects/ledger/features/Implementation/FEAT-7.yaml#item:FEAT-7
  changed-by  FEAT-7  board:projects/ledger/features/Implementation/FEAT-7.yaml#item:FEAT-7
  serves  G-1  board:projects/ledger/charter.yaml#goal:G-1
next: katalogr explain …; … kanbanr why src/lib.rs:13
```

Read it from the top: who uses the symbol (callers and tests), the commit that introduced the line,
the items that annotate it or changed it, and the goals those items serve. `--no-use` leaves out
the first part. The last suggestion, `kanbanr why FILE:LINE`, is the command to run in kanbanr for
the board's own view of the same line; katalogr only suggests it and never runs it.

A reference to an item the board does not have is not hidden. It is reported by `dangling`, which
exits 1:

```text
$ katalogr dangling
annotated-by  ledger:src/lib.rs#fn:unused_helper  src/lib.rs:21  wants board-item:FEAT-99
$ echo $?
1
```

(Here we added `// FEAT-99: retired` above `unused_helper`.) Fix the code, or add the item. By
default `dangling` covers `links-to`, `annotated-by` and `refs-item`; `--kind verified-by` also
checks test names (below), and `--kind` can be repeated.

`katalogr rationale <SYMBOL>` lists what explains a symbol: comment markers (`NOTE:`, `WHY:`,
`RATIONALE:`, upper case), ADR citations, item references and documents. `rationale --hubs <N>`
names the top hubs that have none.

## 3. Reference items from commits

Put a `Refs:` trailer in the commit message:

```text
feat: first ledger

Refs: kanbanr:FEAT-7
```

and index the history:

```text
$ katalogr index --history
queued 1 event(s); watermark 9, 0 pending
```

`--history` indexes the commits that carry item trailers. Each becomes an artifact named
`@history/<commit>`, with a `refs-item` link to the item or requirement. This is what makes the
`commit` and `changed-by` lines appear in `why`. `katalogr index --history --since <REV>` indexes
only the commits after a revision. The reference forms are the same as in code
(`Refs: kanbanr:FEAT-7/R-1` links the requirement).

## 4. Ask from the item

`impact --item <CODE>` starts from a board item and reaches the code, tests and documents tied to
it: the spec, the code that annotates it, and everything its commits touched.

```text
$ katalogr impact --item FEAT-7
impact of board:projects/ledger/…/features-spec/FEAT-7.md#section:record-payments/section:design (item), ledger:src/lib.rs#fn:record (item), … ; project ledger
tests:
  suite  ledger:src/lib.rs#mod:tests  depth 0  extracted  1.0000
  case  ledger:src/lib.rs#mod:tests/fn:records_a_payment  depth 0  extracted  1.0000
```

The first line lists what the item reached (trimmed here); it is long when an early commit touched
many files. See [Impact](../features/impact.md).

`docs-affected` answers the reverse question for documents: which documents are linked to code that
changed since a revision (default: the merge-base with the default branch). See
[Change and history](../features/change-and-history.md).

### Tests that verify requirements

In the item file, a requirement can list tests by name:

```yaml
requirements:
- id: R-1
  text: THE SYSTEM SHALL record every payment it is given.
  tests:
  - name: tests::records_a_payment
```

katalogr links each name to the test with that name in code (a `verified-by` link from the
requirement to the test). `tests` then lists, and `why` marks, the tests that verify a requirement.

```text
$ katalogr tests "board:projects/ledger/features/Implementation/FEAT-7.yaml#item:FEAT-7/req:R-1"
case  ledger:src/lib.rs#mod:tests/fn:records_a_payment  depth 1  extracted  1.0000  verifies board:projects/ledger/…#item:FEAT-7/req:R-1
```

The name must match how the test is named in code. For a Rust test inside `mod tests`, that was
`tests::records_a_payment`; `ledger::tests::records_a_payment` did not match. A name that matches
nothing is reported by `dangling --kind verified-by` (exit 1):

```text
$ katalogr dangling --kind verified-by
verified-by  board:projects/ledger/…#item:FEAT-7/req:R-1  projects/ledger/…/FEAT-7.yaml:11  wants test-name:ledger::tests::records_a_payment
```

When the board changes, run `katalogr index --full` to read it again. In our test, a plain
`katalogr index` did not pick up an edit made in the board repository.

## 5. The contract

Other tools, kanbanr among them, need to know what this binary can do before they ask. `contract`
prints it:

```text
$ katalogr contract
katalogr 0.1.1: api v1, contract 8, schema 1, position format 1, view format 1
capabilities: deps, docs-affected, impact, position, timeline, type-lineage, view-id, views, workspace
```

With `--json` it is one object (`api`, `contract`, `schema`, `position_format`, `view_format`,
`katalogr`, `capabilities`). The same document is served by `GET /api/v1/meta` on
[`katalogr api`](../features/web-and-exports.md).

- A **capability** is a named group of answers. A client checks for the name it needs
  (`impact`, `views`, `docs-affected`, `workspace`, and so on) and does without when it is absent.
- The numbers are versions. A client that understands `contract 8` can rely on the shapes it
  documents; a higher number means a later revision of the contract.
- `position format` and `view format` version the identities below.

### Positions and view IDs

Every answer carries `positions` (one entry per repo: the commit and, when there are uncommitted
changes, a `dirty` hash) and `exact`, which is true when the index reflects those positions.
`katalogr position` prints the checkout's position and what the index reflects. Both sides agree
on the format, so a board can store "the code at this position" and ask for it later with
`--at`.

A view (see [Views and diagrams](../features/views-and-diagrams.md)) also carries a `view_id`, such
as `v1-8fb5a7f7…`. It is computed from the position, the facts version and the question, not from
the binary version, so two tools that ask the same question of the same code get the same ID.
`katalogr view … --fresh` recomputes a view and exits 1 when the stored one differs.

## 6. Query blocks in board documents

A fenced block with the info string `katalogr-query` holds one line: a view and its flags.

````text
```katalogr-query
structure --level package --max-nodes 40
```
````

The ops are `structure`, `contracts`, `callers <symbol>`, `impact`, `lineage`, `path <from> <to>`
and `cycles`. kanbanr uses the same block syntax, so a block works in both. Without katalogr,
a reader sees the block's source as it stands.

- **`katalogr view check <PATH>`** lists every block under a folder (or one Markdown file) that
  does not parse, with file and line. It reads no index and exits 1 when a block is bad:

  ```text
  $ katalogr view check docs
  all query blocks parse
  $ katalogr view check badq
  x.md:1: bad view: unknown op `bogus`; known ops: structure, contracts, callers, impact, lineage, path, cycles
  ```

- **`katalogr view snapshot --docs <DIR> --out <DIR>`** writes the view of every block in the
  documents under `--docs`, for one position (`--at`, else the current checkout), into `--out`. It
  is all or nothing: if any view is not exact, or no context holds the position, nothing is written
  and the command exits non-zero. It never changes your documents.

  ```text
  $ katalogr view snapshot --docs docs --out snap
  snapshot: 1 block(s), 1 view(s), 2 repo(s)
  ```

  The folder holds `index.json` (the positions, the repos and one entry per block with its view
  ID), and a `.json` and `.md` file per view.

kanbanr runs `katalogr view snapshot` when it stores a release, if katalogr is installed; a missing
katalogr, or a failed snapshot, is a warning and does not stop the release. To read stored
snapshots later, `katalogr view timeline --snapshots <DIR>` lists them and
`katalogr view stored <ID> --snapshots <DIR>` prints one. See
[Views and diagrams](../features/views-and-diagrams.md).

Links into the web view use a stable form,
`<serve url>/#/<view>?at=<position>&symbol=<name>`, with `<view>` one of `graph`, `service-map` or
`impact`. See [Web view and exports](../features/web-and-exports.md).

## Summary of commands

| You want | Run |
|---|---|
| Add the board to the project | edit `config.yaml`, then `katalogr project add-root` and `katalogr index --full` |
| Why a line exists | `katalogr why FILE:LINE` |
| What an item reaches | `katalogr impact --item CODE` |
| Item references that name nothing | `katalogr dangling` |
| Tests that verify a requirement | `katalogr tests <requirement>` |
| Commits that carry `Refs:` | `katalogr index --history` |
| What this binary supports | `katalogr contract` |
| Check query blocks | `katalogr view check <PATH>` |
| Store the views of a document set | `katalogr view snapshot --docs <DIR> --out <DIR>` |

---

What changed in each version is in the [release notes](../releases/index.md).
