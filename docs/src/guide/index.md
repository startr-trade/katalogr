# User guide

katalogr builds a graph of your code and documents, and answers questions from it: where something
is defined, what refers to it, what a change reaches and why a line exists. This guide shows how to
set it up and use it. Every example runs the built `katalogr` binary on a small invented project
named `ledger`, so the output you see is real. Paths in output are trimmed with `…`.

If you have not installed katalogr yet, start with [Install](../install.md). The version this guide
describes is the 0.2 line; see the [release notes](../releases/index.md) for what changed.

## What katalogr is for

- **Finding.** Where is this symbol? Who calls it? Which tests cover it?
- **Judging a change.** What does this edit reach, and which documents now need an update?
- **Understanding.** Why does this line exist? Which board item, commit or note explains it?
- **Showing.** Draw the structure, the cycles and the callers of a symbol, or serve them in a web
  view.
- **Keeping watch.** Dependency cycles, dead code, dangling references and index coverage, with
  exit codes that work as CI gates.

Every command works from the command line, in CI and in an editor. Agent integration speeds work up
and never gates it.

## Concepts

**Kindex.** A directory (a git repository) that holds the index data and settings of one or more
projects. It lives apart from your code, so nothing is written into your repositories except a small
marker file. Create one with `katalogr kindex init`.

**Project.** A named set of repositories that are indexed together. A project's settings live in
the kindex, in `projects/<name>/config.yaml`.

**Repository (repo).** One checkout in a project, with a short name such as `ledger`. Symbols are
named `<repo>:<path>#<kind>:<name>`, for example `ledger:src/lib.rs#fn:record`. You can give a
plain name such as `record` to most commands; katalogr resolves it, and says so when it is
ambiguous (exit 3).

**The marker.** A file named `.katalogr` at the root of a repository. It tells katalogr which
kindex, project and repo this checkout belongs to, so commands run from anywhere inside it need no
flags. katalogr adds it to `.git/info/exclude` so it is never committed.

**The index and the queue.** Changes (from hooks, `notify`, `watch` or `index`) go into a queue. The
index is the graph built from the queue. A query that needs a fresh index waits for the queue to
drain, by default.

**Contexts and positions.** A context is one reader's view of the index: the base, each checkout's
own (including linked git worktrees) and pinned commits. A position names a state of the code:
`<commit>`, or `<commit>+<dirty>` when there are uncommitted changes, where the second part is a
hash of those changes. `--at <POSITION>` answers for that state. See
[Change and history](../features/change-and-history.md).

**Links, kinds and confidence.** The graph is made of symbols and links such as `calls`,
`links-to`, `annotated-by` and `refs-item`. Each link has a confidence: `Extracted` (read straight
from the source), `Inferred` (worked out by a rule) or ambiguous (resolved to several targets).
Commands that follow links group by kind and confidence, and several can leave out weak links
(`--strict`).

**Freshness.** Every answer reports the index watermark, pending changes and whether it is stale. By
default a read waits until the index is fresh. `--consistency stale-ok` answers from the index as it
is. `--consistency wait-all` waits for every project's index. `--timeout <SECONDS>` bounds the
wait.

**Absence is not proof.** A query that finds nothing says so; one that was cut off says `partial`.
Treat "no path" and "no callers" as "none found within what was indexed".

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | The command found what it is asked to fail on (a cycle, a dangling reference, a finding, a difference) |
| 2 | Usage or configuration error |
| 3 | The name is ambiguous |
| 4 | Nothing matched, or there is no record |
| 5 | A refusal: stale or inexact answer where exactness was required, or an incompatible record |
| 6 | The position is not held by any context |

Individual pages note the codes that matter for each command.

## Output options that apply everywhere

| Flag | Effect |
|---|---|
| `--json` | Machine-readable output, one JSON document |
| `--budget <TOKENS>` | Cut output to about this many tokens |
| `--at <POSITION>` | Answer for this position |
| `--context <NAME>` | Read this context (`current`, `base` or an id from `context list`) |
| `--consistency <MODE>`, `--timeout <SECONDS>` | Freshness: `wait`, `wait-all` or `stale-ok` |
| `--fresh` | Compute the answer again instead of reading a stored one |
| `--no-peers` | Read this project alone, even when it names peers |
| `--kindex <PATH>`, `--project <NAME>` | Say which kindex and project, when no marker is found |

## Map of the pages

**Guide**

1. [Getting started](getting-started.md): create a kindex and a project, index, ask a first
   question, keep the index current.
2. [Using katalogr with Claude Code](claude-code.md): skill, hooks, `CLAUDE.md` block.
3. [Connecting to kanbanr](kanbanr.md): board references in code, the versioned contract, query
   blocks in board documents.
4. [Using katalogr from other agent hosts (MCP)](mcp.md): the stdio server and its tools.

**Features**

- [Navigation](../features/navigation.md): search, find, show, explain, refs, callers, tests, path,
  why, map, replace.
- [Impact](../features/impact.md): what a change reaches.
- [Lineage](../features/lineage.md): where a type or contract is declared and goes; mirrored
  copies.
- [Dependencies](../features/dependencies.md): packages pinned by lockfiles.
- [Structure and health](../features/structure.md): hubs, communities, cycles, dead code,
  coverage, rationale, dangling references, stored answers.
- [Views and diagrams](../features/views-and-diagrams.md): render, view, query blocks.
- [Web view and exports](../features/web-and-exports.md): serve, api, static exports.
- [Change and history](../features/change-and-history.md): diff, docs-affected, contexts,
  positions.
- [Workspaces](../features/workspaces.md): peer projects and cross-project links.
- [Index lifecycle](../features/index-lifecycle.md): index, status, doctor, snapshots, audit
  records, shards, profiles.
- [Retrieval evaluation](../features/retrieval-eval.md): golden sets, baselines, gates, sweeps.

## Coming

Further capabilities, such as vulnerability findings for pinned packages, are planned. They have no
page here until they are built; each adds its page when it ships.
