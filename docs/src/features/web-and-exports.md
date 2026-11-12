# Web view and exports

**Questions answered:** can I look at the graph in a browser, can other programs read the same
answers over HTTP, and can I hand a reviewer a file that needs no server?

## serve: the web view

```text
katalogr serve [--listen ADDR] [--workers N] [--queue N] [--no-process] [--cors-origin ORIGIN]
               [--snapshots DIR]...
```

`katalogr serve` runs a read-only web view and the HTTP/JSON API on one address and prints it:

```text
$ katalogr serve
katalogr serve: listening on http://127.0.0.1:7878
```

Open that address in a browser. The default address is `127.0.0.1:7878`; only loopback addresses
are served, so the view is not reachable from other machines. It needs no board, no agent and no
toolchain.

What the page shows:

| View | Shows |
|---|---|
| Graph | Any [view](views-and-diagrams.md) as a diagram or an explorable graph (click to select, double-click to add callers) |
| Service map | The contracts and who declares them |
| Impact | What a change or symbol [reaches](impact.md) |
| Health | Hubs, cycles, communities, dead code and coverage ([structure](structure.md)) |
| Dependencies | The packages your lockfiles pin ([dependencies](dependencies.md)) |
| Type lineage | Where a type goes ([lineage](lineage.md)) |
| Symbol and search | A symbol's explanation, tests, callers and source; a search box |

A view whose capability the binary lacks says so on the page instead of failing. The time slider
reads stored views and snapshot views (`--snapshots <DIR>`, repeatable, adds snapshot folders from
[`view snapshot`](views-and-diagrams.md#view-snapshot)).

A page can be linked: `http://127.0.0.1:7878/#/graph?at=<position>&symbol=<name>`, where the view
is `graph`, `service-map` or `impact`. Unknown parameters are ignored.

Options:

| Option | Effect |
|---|---|
| `--listen <ADDR>` | Address to listen on (default `127.0.0.1:7878`) |
| `--workers <N>` | Worker threads (default: the core count, at most 8) |
| `--queue <N>` | Requests that may wait for a worker; more get 503 (default 64) |
| `--no-process` | A read never processes the queue (the hosted form) |
| `--cors-origin <ORIGIN>` | Send CORS headers for exactly this origin |
| `--snapshots <DIR>` | A snapshot folder (or a folder of them) for the time slider; repeatable |
| `--write-assets <DIR>` | Write the page (`index.html` and `assets/`) into a folder and exit; needs no project |
| `--api-base <URL>` | With `--write-assets`: the API address the written page calls, in place of the same origin |

`--write-assets` lets you host the page on a static server and point it at an API elsewhere.
`--api-base` is only valid with it (exit 2 otherwise).

## api: the HTTP/JSON API

`katalogr api` serves the same read queries without the page, for scripts and other tools. It takes
the same options as `serve`, except the two that write the page.

```text
$ curl -s "127.0.0.1:7878/api/v1/find?text=record"
{"schema":1,"watermark":11,"pending":0,"stale":false,"truncated":null,"result":[{"name":"ledger:src/lib.rs#fn:record",…}],"next":[…],"positions":[…],"exact":true,"view_id":"v1-…"}
```

- The routes are under `/api/v1/`. The list, with parameters and shapes, is the OpenAPI document at
  `/api/v1/openapi.json`. It includes a route for every read command (`find`, `refs`, `callers`,
  `explain`, `show`, `search`, `path`, `why`, `tests`, `map`, `hubs`, `communities`, `cycles`,
  `dead-code`, `coverage`, `rationale`, `dangling`, `deps`, `docs-affected`, `impact`, `lineage`,
  `mirrors`, `position`, `status`, `doctor`, the context and workspace reads and the `views/<op>`
  routes), plus `meta` (the [contract](../guide/kanbanr.md#5-the-contract)), `timeline` and
  `stored-view`.
- The query parameters carry the command's flags, with `consistency`, `timeout`, `budget` and `at`.
- Errors are JSON, with the HTTP status mapped from the failure (404 `no-match`, and so on):

  ```text
  {"error":"no-match","message":"no symbol matches \"nosuch\"","schema":1}
  ```

- A `budget` smaller than the answer cuts it and says so in `truncated` and `partial`.
- The server holds no state of its own and answers from the index. With `--no-process` a read never
  processes the queue, which suits a copy of the index hosted for many readers.

## export html: one file for reviewers

```text
katalogr export html --out <FILE> [--at POSITION] [--max-symbols N] [--with-source]
```

Writes one self-contained file that holds the web view and the answers it needs, for a position. A
reviewer opens it in a browser with no server and no katalogr.

```text
$ katalogr export html --out review.html
katalogr: export: wrote 1250915 bytes
katalogr: export: repos named: board, ledger
```

- The file shows the views as they were at that position. The time slider says the file is fixed.
  A view or symbol that was not exported says *This isn't in this export*.
- `--max-symbols <N>` keeps only the N most connected symbols, and says so:
  `export: holds the 5 most connected of 30 symbols (--max-symbols)`.
- `--with-source` embeds the source lines. By default only signatures and docs are embedded.
- It stops, writing nothing, when the position is not held (exit 6), when an answer is stale or not
  exact (exit 5), or when an answer holds an absolute project path (exit 1).
- It contains symbol names, paths and signature text. Treat it as private as the code.

## export graph: one view for graph tools

```text
katalogr export graph <OP> [ARGS]... [--format dot|graphml|json] --out <FILE>
```

Writes one [view](views-and-diagrams.md) as DOT, GraphML or Cytoscape JSON, for Graphviz, Gephi,
yEd or Cytoscape.

```text
$ katalogr export graph cycles --format graphml --out cycles.graphml
katalogr: export: wrote 960 bytes
$ katalogr export graph callers add --format json --out callers.json
katalogr: export: wrote 862 bytes
```

The JSON begins `{"format":"katalogr-graph","format_version":1,"title":"callers add --depth 2 --max-nodes 60","shown":3,"total":3,"elements":{"nodes":[…`.

See also: [Views and diagrams](views-and-diagrams.md) for `render`, which writes a diagram to
standard output.

---

What changed in each version is in the [release notes](../releases/index.md).
