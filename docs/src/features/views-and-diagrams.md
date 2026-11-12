# Views and diagrams

**Question answered:** can I see this as a picture, in a format my tools read, and can I keep that
picture true as the code changes?

A **view** is a question about the graph that has a graph-shaped answer. The question is an **op**
and its flags:

| Op | Draws | Main flags |
|---|---|---|
| `structure` | How packages or files depend on each other | `--level file\|package` (default package), `--max-nodes N` (default 60), `--inferred` |
| `contracts` | The contracts and who declares them | `--kind K`, `--max-nodes N` |
| `callers <symbol>` | Who calls a symbol | `--depth N` (default 2), `--max-nodes N` |
| `path <from> <to>` | The shortest chain between two symbols | `--max-len N` (default 6) |
| `cycles` | Dependency cycles | `--level file\|package`, `--limit N` (default 5) |
| `impact`, `lineage` | What [impact](impact.md) and [lineage](lineage.md) reach | as those commands |

A view is cut to `--max-nodes` nodes, keeping the most central, and says so when partial.

## render

Draw a view in a diagram format. The op comes first.

```text
katalogr render <OP> [ARGS]... [--format mermaid|dot|graphml|structurizr|svg]
```

```text
$ katalogr render callers add
flowchart LR
%% katalogr view: callers add --depth 2 --max-nodes 60
  n1d5b857c8596["totals.rs/fn:report"]
  nf24099caf4ea["totals.rs/fn:add"]
  nf77d4a4312a1["lib.rs/fn:record"]
  n1d5b857c8596 -->|"calls"| nf24099caf4ea
  nf77d4a4312a1 -->|"calls"| nf24099caf4ea
```

The default format is Mermaid, which GitHub, mdBook and many editors draw. The other formats go to
Graphviz (`dot`), graph tools (`graphml`), Structurizr (`structurizr`) and a standalone picture
(`svg`, laid out by katalogr, with no script in it). Node identifiers are stable, derived from the
canonical name, and the output is sorted, so the same code gives the same bytes. Inferred links are
drawn dashed in Mermaid. A view that spans several repos draws one group per repo.

```text
$ katalogr render structure --format svg > structure.svg
```

### Embed views in documents

A fenced block with the info string `katalogr-query` holds one line, an op and its flags:

````text
```katalogr-query
structure --level package --max-nodes 40
```
````

Two commands replace such blocks with the drawn view.

- **`katalogr render filter`** reads Markdown on standard input and writes it with each block
  replaced. `--assets inline` (default) embeds the drawing script once; `--assets <DIR>` writes it
  to a folder and links it. A document with no block is written back unchanged. A block that fails
  becomes a visible error box with its source and the message, and the run still exits 0, so one bad
  block does not stop a documentation build. `--strict` exits non-zero on the first failure.

  ```text
  $ katalogr render filter < docs/overview.md > overview.html
  ```

- **`katalogr render mdbook`** is an mdBook preprocessor. Add it to `book.toml`:

  ```toml
  [preprocessor.katalogr]
  command = "katalogr render mdbook"
  ```

  It replaces the blocks in every chapter and copies its assets into the book's source folder
  (`katalogr-assets/`) when they differ. For any renderer other than `html`, it reports it does not
  support it, and mdBook leaves the blocks as source.

katalogr finds the project from the directory `mdbook` runs in, so build the book from inside a
marked repository (or set `KATALOGR_KINDEX` and `KATALOGR_PROJECT`).

## view

`katalogr view <OP> [ARGS]...` returns the same view as Mermaid plus data, and stores it by its
**view ID**. The ID is computed from the position of the code, the facts version and the question,
so the same question about the same code is answered from the store with no query.

```text
$ katalogr view cycles
flowchart LR
%% katalogr view: cycles --level package --limit 5
katalogr: view v1-1501aa9621605bc6d13d22811411d2d1
```

The view ID is printed after the diagram. With `--json` the answer holds `view_id`, `exact`,
`positions`, `query`, `mermaid` and `graph` (nodes and edges), the same as the HTTP route
`GET /api/v1/views/<op>`.

- Only an **exact** answer is stored: one for a position the index fully reflects.
- `katalogr view <OP> --fresh` computes the view again and compares it with the stored one. It exits
  1 on a difference, which makes it a check that nothing drifted.

### view check

Report every `katalogr-query` block under a folder or one Markdown file that does not parse, with
file and line. It reads no index and exits 1 when a block is bad.

```text
$ katalogr view check docs
all query blocks parse
$ katalogr view check badq
x.md:1: bad view: unknown op `bogus`; known ops: structure, contracts, callers, impact, lineage, path, cycles
```

### view snapshot

Write the view of every block under `--docs` for one position into `--out`. The position is `--at`,
or the current checkout. It is all or nothing: a view that is not exact, or a position no context
holds, writes nothing and exits non-zero. It never changes your documents.

```text
$ katalogr view snapshot --docs docs --out snap
snapshot: 1 block(s), 1 view(s), 2 repo(s)
$ ls snap
index.json  v1-938dc844a9e18729cba7167ca6254f6d.json  v1-938dc844a9e18729cba7167ca6254f6d.md
```

`index.json` lists the positions, the repos and, per block, the document, line, source and view ID.
Keep snapshots with your release to show what the structure looked like then.

### view timeline and view stored

`katalogr view timeline` lists the stored views and snapshot views, oldest first. This is what the
web view's time slider shows. It reads no index and waits for no queue. `--snapshots <DIR>` adds a
snapshot folder and `--limit <N>` cuts the list.

```text
$ katalogr view timeline --snapshots snap
2026-11-12 04:28  4f38aa24  stored-view  cycles
2026-11-12 04:28  4f38aa24  stored-view  callers
2026-11-12 04:28  4f38aa24  snapshot  structure
```

Columns: when it was stored, the short commit, where it came from (`stored-view` or `snapshot`) and
the op. `katalogr view stored <ID>` prints a stored view by its ID, as `view` does; add
`--snapshots <DIR>` to look in a snapshot folder. A missing ID is exit 4.

See also: [Web view and exports](web-and-exports.md) for the same views in a browser, and
[Connecting to kanbanr](../guide/kanbanr.md#6-query-blocks-in-board-documents).

---

What changed in each version is in the [release notes](../releases/index.md).
