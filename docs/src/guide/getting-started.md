# Getting started

This page takes a project from nothing to a first answer, then shows how to keep the index current.
The examples use a small invented project named `ledger`: a Rust crate (`src/lib.rs`,
`src/totals.rs`, `src/rules.rs`, `src/audit.rs`), a TypeScript client (`web/client.ts`), a
`Cargo.lock` that pins one package, and a Markdown note (`docs/overview.md`). It is a git
repository with two commits. The same project is used on every page of this guide.

`src/totals.rs`:

```rust
/// Add an amount to the running total.
pub fn add(cents: i64) -> i64 {
    cents
}

pub fn report() -> i64 {
    add(0)
}
```

## 1. Create a kindex

The kindex holds the index and the settings. Keep it beside your code, not inside it.

```text
$ katalogr kindex init kx
kindex ready at …/kx
```

The command is safe to run again. The kindex is a git repository; commit the project settings in
it when you want to share them. The index itself is a cache and is ignored by git.

## 2. Create a project and mark the repository

```text
$ katalogr project init ledger --repo ledger=./ledger --mark --kindex kx
project ledger created at …/kx/projects/ledger
marker written: …/ledger/.katalogr
```

`--repo NAME=PATH` names each repository of the project (repeat it for more than one). `--mark`
writes the marker file `.katalogr` into the repository the command runs in. Its content is three
lines:

```text
kindex: ../kx
project: ledger
repo: ledger
```

From now on, any `katalogr` command run inside the repository finds the kindex and project by
itself. Elsewhere, pass `--kindex <PATH>` and `--project <NAME>`, or set `KATALOGR_KINDEX` and
`KATALOGR_PROJECT`.

`project init` fails with exit 2 when the project already exists. To add a repository to an
existing project, add an entry under `repos:` in `projects/<name>/config.yaml`, then record where it
lives on this machine:

```text
$ katalogr project add-root board /path/to/board
root of board recorded
```

`project add-root NAME PATH` stores the location of a repository on this machine only; the
location is never committed to the kindex. Each repository entry has `include` and `exclude` globs;
the default excludes `.git/**`, `target/**` and `node_modules/**`.

## 3. Index

```text
$ katalogr status
katalogr: warning: no index yet: run katalogr index --full
index status; watermark 0, 1 pending (stale); context w-74e602dc4f11f0c9

$ katalogr index
queued 2 event(s); watermark 6, 0 pending

$ katalogr status
index status; watermark 6, 0 pending; context w-74e602dc4f11f0c9
```

`katalogr index` queues what changed and brings the index up to date. Useful forms:

- `katalogr index --full` rescans every repository of the project.
- `katalogr index --since <REV>` indexes the changes since a revision, up to `HEAD`;
  `--range A..B` does the same for a range.
- `katalogr index --history` also indexes the commits that carry item trailers (`Refs:`). See
  [Connecting to kanbanr](kanbanr.md).
- `katalogr index --no-reports` skips the diff reports that merges asked for.

`katalogr status` shows the watermark (a counter that rises with every processed change), the
pending changes and the context. `pending` above zero with `(stale)` means a query will wait for the
queue unless you pass `--consistency stale-ok`.

## 4. Ask a first question

```text
$ katalogr find record
fn  ledger:src/lib.rs#fn:record  src/lib.rs:13
fn  ledger:src/lib.rs#mod:tests/fn:records_a_payment  src/lib.rs:28
next: katalogr explain ledger:src/lib.rs#fn:record; katalogr show ledger:src/lib.rs#fn:record; …

$ katalogr explain record
fn  ledger:src/lib.rs#fn:record  src/lib.rs:13
signature: pub fn record(p: &Payment) -> u64
doc: Record a payment and return its id.
outgoing (2):
  annotated-by  board:projects/ledger/features/Implementation/FEAT-7.yaml#item:FEAT-7  [Extracted]
  calls  ledger:src/totals.rs#fn:add  [Extracted]
incoming (1):
  calls  ledger:src/lib.rs#mod:tests/fn:records_a_payment  [Extracted]
```

Each line is: kind, canonical name, file and line. The `next:` line suggests follow-up commands. Add
`--json` to any command to get the same answer as data, with the index watermark, the positions it
reflects and an `exact` flag. The full set of questions is on the
[Navigation](../features/navigation.md) page.

## 5. Keep the index current

Four ways, from lightest to most complete:

1. **Git hooks.** `katalogr hooks install` adds `post-commit`, `post-checkout` and `post-merge`
   hooks that queue what changed (`katalogr notify --from-git`). They never fail or block git.
2. **Claude Code hooks.** The same command adds Claude Code settings so an edit is queued and
   reindexed straight away. See [Using katalogr with Claude Code](claude-code.md).
3. **`katalogr watch`.** Polls the checkout's position and reindexes when it changes, until you
   interrupt it. Use it for edits made in an editor. `--interval <SECONDS>` sets the poll
   (0.1 to 3600; default 2).
4. **By hand.** `katalogr notify <PATHS>...` queues named files (`--deleted` for deleted files),
   and `katalogr index` processes the queue.

```text
$ katalogr notify src/lib.rs
queued 1 change(s)
$ katalogr status
index status; watermark 6, 1 pending (stale); context w-b7c0cb9552a6798d
$ katalogr index
queued 1 event(s); watermark 10, 0 pending
```

A read waits for the queue to drain by default (see freshness in the [overview](index.md)), so a queued change is visible to the next query.

## 6. Check health

`katalogr doctor` checks the index and prints a fix for each problem. It exits 1 when it finds a
problem at or above `--fail-on` (default `warning`).

```text
$ katalogr doctor
katalogr doctor: katalogr 0.1.1, shard format 2, cache schema 15
info    KD-GIT-003  the kindex repository has no commit yet
        fix: git -C <kindex-dir> add projects/ledger && git -C <kindex-dir> commit -m "update project"
summary: 0 error, 0 warning, 1 info; 10 checks (1 with findings, 0 skipped)
```

## Where next

- [Navigation](../features/navigation.md) for everyday questions.
- [Impact](../features/impact.md) before you change something.
- [Using katalogr with Claude Code](claude-code.md) or [with other agent hosts](mcp.md).
- [Index lifecycle](../features/index-lifecycle.md) for `doctor`, snapshots and audit records.

---

What changed in each version is in the [release notes](../releases/index.md).
