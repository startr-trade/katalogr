# Using katalogr from other agent hosts (MCP)

`katalogr mcp` serves katalogr's queries over the Model Context Protocol on standard input and
output, for agent hosts that cannot use the Claude Code [skill and hooks](claude-code.md). It holds
no query logic of its own: each tool runs the same command you would run in a terminal, and the
text it returns is the same JSON that `--json` gives.

Use it with any MCP host that can start a stdio server. The server only reads and indexes; it
cannot edit your source files, settings or hooks.

## Configure the host

Add a stdio MCP server entry to the host's configuration. The exact file and syntax belong to the
host; the entry is always the same three things, a command, its arguments and (if the host wants
one) a name:

```json
{
  "mcpServers": {
    "katalogr": {
      "command": "katalogr",
      "args": ["mcp", "--root", "/path/to/project"]
    }
  }
}
```

`--root <DIR>` is the directory the project is found from (the folder that holds, or sits below, the
`.katalogr` marker). It matters because hosts start servers in varying working directories. Without
it, the current directory is used. If your project has no marker, pass `--kindex <PATH>` and
`--project <NAME>` as additional arguments. `katalogr` must be on the host's `PATH`, or give the full
path to the binary as the `command`.

Other options:

| Option | Effect |
|---|---|
| `--root <DIR>` | Where the project is found from (default: the current directory) |
| `--output-schemas` | Include each query tool's `schema/v1` file as its `outputSchema` |
| `--kindex <PATH>`, `--project <NAME>` | Say which kindex and project |
| `--consistency <MODE>`, `--timeout <SECONDS>` | Freshness for every call (see below) |
| `--budget <TOKENS>` | Cut every answer to about this many tokens; a tool's own `budget` argument overrides it |

Hosts that mix other servers' tools into the same context pay for every tool schema, so register
katalogr only where you use it.

## Check it by hand

The server reads one JSON-RPC message per line. You can talk to it from a terminal:

```text
$ printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | katalogr mcp --root .
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"katalogr","version":"0.1.1"},"instructions":"katalogr answers from a prebuilt index of this project. Start with find or search, then explain, show, refs or callers. Results are katalogr schema/v1 JSON; each may list next steps."}}
{"jsonrpc":"2.0","id":2,"result":{"tools":[ … ]}}
```

Only `tools` is advertised. Resource and prompt lists are empty.

## The tools

This is the list the server returned to `tools/list`. Required arguments are marked with `*`. Every
query tool also accepts the optional `at` (a position), `budget` and `context` arguments described
under [Freshness and errors](#freshness-and-errors); `index` and `status` do not.

| Tool | Answers | Arguments |
|---|---|---|
| `find` | Symbols whose name matches the text | `text`* |
| `refs` | What references a symbol, including through re-exports | `symbol`*, `expand` |
| `callers` | Callers of a symbol, up to `depth` hops | `symbol`*, `depth`, `expand` |
| `explain` | A symbol's signature, doc, links and rationale | `symbol`*, `per_side` |
| `show` | A symbol's source lines, or only its signature and doc | `symbol`*, `signature` |
| `path` | The shortest chain of links between two symbols | `from`*, `to`*, `max_len` |
| `map` | The project outline, ranked by PageRank | none |
| `search` | Symbols ranked by BM25 over names, docs, sections and contract keys | `terms`*, `limit` |
| `dangling` | References that name something that does not exist | `kind` |
| `deps` | Package versions the lockfiles pin, who pins them and who uses them | `package`, `limit`, `used_by_limit` |
| `why` | The symbol at `FILE:LINE` and the rationale linked to it | `location`* |
| `docs-affected` | Documents linked to code that changed since a revision | `since`, `kind`, `limit` |
| `impact` | What a symbol, file, range, change or item reaches, and the tests over it | `text`, `file`, `range`, `since`, `item`, `depth`, `test_depth`, `limit` |
| `lineage` | What a type, schema element, symbol or contract is and where it goes | `text`*, `depth`, `limit`, `per_group`, `expand` |
| `index` | Queue changes and bring the index up to date | `full`, `history`, `range`, `since` |
| `status` | The index watermark, pending changes and staleness | none |

The arguments match the command's flags (`per_side` is `--per-side`, `max_len` is `--max-len`).
`impact` takes at most one of `text`, `file`, `range`, `since` and `item`. See
[Navigation](../features/navigation.md), [Impact](../features/impact.md),
[Lineage](../features/lineage.md) and [Dependencies](../features/dependencies.md) for what each
answer means.

Commands that are not tools: `replace`, `notify`, `hooks`, `claude`, `skill`, `project` and
`kindex` (they change files), and the structure and view commands (`hubs`, `cycles`, `render`,
`view` and the rest). Run those from a terminal, or serve them over HTTP with
[`katalogr serve`](../features/web-and-exports.md).

## A call

```text
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"find","arguments":{"text":"record"}}}
```

The reply carries the answer as one text content item. It is the same document as
`katalogr find record --json`:

```text
{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"{\"schema\":1,\"watermark\":11,\"pending\":0,\"stale\":false,\"truncated\":null,\"result\":[{\"name\":\"ledger:src/lib.rs#fn:record\",\"kind\":\"fn\",\"path\":\"src/lib.rs\",\"span\":{…}}, …],\"next\":[\"katalogr explain ledger:src/lib.rs#fn:record\", …],\"positions\":[…],\"exact\":true,\"view_id\":\"v1-…\"}"}]}}
```

`result` holds the answer, `next` lists follow-up commands (as command lines, which the agent can
turn into tool calls), and `positions`, `exact` and `view_id` say which state of the code it
describes.

## Freshness and errors

- **Freshness.** Every call reports `watermark`, `pending` and `stale`. By default a read waits
  until the index is fresh, as on the command line. Start the server with
  `--consistency stale-ok` to answer at once from the index as it is. Call `index` (or keep the
  index current with [hooks or `watch`](getting-started.md#5-keep-the-index-current)) so answers
  match the code.
- **`at`** answers for a position (see [Change and history](../features/change-and-history.md)).
- **Errors.** A failed call returns `isError: true`, the message as text, and the command's exit
  code in `_meta`:

  ```text
  {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"no symbol matches \"nosuch\""}],"isError":true,"_meta":{"katalogr/exit":4}}}
  ```

  The exit codes are the ones in the [overview](index.md#exit-codes): 3 means ambiguous, 4 means
  nothing matched.
- **Calls are serial.** Two calls never run at once. A call that the host cancels is not started if
  it has not begun.
- **Limits.** One request line may be at most 8 MiB.

The server reads the same index as the command line, so a terminal and an agent host can use one
project at the same time.

---

What changed in each version is in the [release notes](../releases/index.md).
