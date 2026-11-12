# Change and history

**Questions answered:** what changed in the structure between two points, which documents does a
change leave out of date, how do I read the code as it was, and how do several checkouts and
worktrees stay apart?

## diff

Report what changed in the code's structure between two commits, branches or releases: symbols
added, removed, moved or changed, links, contracts, and shifts in hubs, cycles, communities and
coverage.

```text
katalogr diff [A] [B] [--repo NAME] [--format markdown|html] [--limit N]
              [--out FILE | --out-dir DIR] [--requested]
```

```text
$ katalogr diff HEAD~2 HEAD
# Changes in ledger from 19714ed1e468 to fb35bc3e337f

## Summary

Repository `ledger`, from `19714ed1e468` to `fb35bc3e337f`. Facts version `d87d1cd1d332`. Each list is limited to 200 entries.

| Measure | Value |
| --- | --- |
| Files compared | 3 |
| Symbols added | 7 |
| Symbols removed | 0 |
| Symbols moved | 0 |
| Symbols with a changed signature | 0 |
| Links added | 5 |
…
| Cycles | 0 to 1 |
…
```

The report is Markdown by default (`--format html` for a page). The report is stored and read back,
so the second ask is instant; `--json` gives the data. `--limit` is entries per list (default 200).
`--out <FILE>` writes the report to a file and `--out-dir <DIR>` writes `diff-<a12>-<b12>.json`,
`.md` and `.html` into a folder; both are refused where git would track the file, to keep reports
out of your repository by accident. `--repo` picks the repository (default: the current one).
`katalogr diff --requested` builds the reports that merges asked for and stops; `index` does this
too unless you pass `--no-reports`.

## docs-affected

List documents linked to code that changed since a revision (default: the merge-base with the
default branch), up to `--at` (default: this checkout, uncommitted changes included).

```text
katalogr docs-affected [--since REV] [--kind KIND]... [--limit N]
```

```text
$ echo "// tweak" >> src/totals.rs
$ katalogr docs-affected
ledger:docs/overview.md  docs/overview.md
  links-to  ledger:docs/overview.md#section:ledger-overview -> ledger:src/totals.rs  modified file
```

Each document is followed by the links that tie it to changed code, and what changed (`modified
file`, or a symbol). `--kind` chooses the link kinds that count as "documents this code" (default:
the project's `docs.link_kinds` setting, else `links-to`, `documented-by`, `annotated-by` and
`specifies`). `--since` takes a revision or a clean position; a position with uncommitted changes is
refused. Use it in review or CI to find documents to update. No output means no linked document
changed.

## Positions and `--at`

A **position** names a state of a checkout: the commit, plus a hash of any uncommitted changes.

```text
$ katalogr position
fb35bc3e337f073b998f559be5af643a02d01c9f
reflects: 4f38aa2404c0cd4b359239bfb7d51fd799322cd3 (other repo)
reflects: fb35bc3e337f073b998f559be5af643a02d01c9f
exact: true
```

The first line is the checkout's position. `reflects:` lists the positions the index holds for each
repo of the project; `exact: true` means the index describes this checkout exactly. After an edit
the position gains `+<dirty>` and, until the index catches up, `exact: false`:

```text
$ katalogr position
1514e9acba3e738fe9c87f197b5b885c7aaf2ae3+40f010fd46c5…
reflects: 1514e9acba3e738fe9c87f197b5b885c7aaf2ae3+d804774ceb27…
exact: false
```

`position` works without a project (use `--repo <REPO>` to choose the repo; default: the marked one).

`--at <POSITION>` on any command answers for that position: `<commit>` (40 lowercase hex digits),
`<commit>+<dirty>` or `none+<dirty>`. The position must be held by some context. If none holds it,
the command exits 6.

```text
$ katalogr find record --at 19714ed1e468c5d45da0e94efabc536e8c7e076e
fn  ledger:src/lib.rs#fn:record  src/lib.rs:11
…
```

(Write the full 40-digit commit; the short form is refused with exit 2.) The answer then describes the
code as it was at that commit, including a function's earlier line number.

## Contexts

A **context** is one reader's view of the index. There is always the **base**. Each checkout has its
own **current** context, named `w-<id>`, so edits in one checkout do not disturb another. A
**pinned** context, named `c-<id>`, reads one commit straight from git objects with no checkout.

```text
$ katalogr context list
  base base 299043 bytes
  c-13f46a4badbf0abf commit ledger @1514e9acba3e 213213 bytes
* w-74e602dc4f11f0c9 checkout ledger 375273 bytes
```

The star marks the context a command reads by default. Select another with `--context <NAME>` or
the `KATALOGR_CONTEXT` environment variable. Commands:

| Command | Does |
|---|---|
| `context list` | List the base and every context on this machine (no paths) |
| `context show <ID>` | Show one context |
| `context add --commit <REV> [--repo <NAME>]` | Add a pinned context that reads a commit from git objects |
| `context diff <A> <B> [--limit N]` | Compare two contexts' graphs: links, dangling references, measures |
| `context conflicts <A> <B>` | Contracts that both contexts changed against the base; exits 1 when there are any |
| `context prune [--keep N]` | Remove contexts down to `contexts.max` (default 8): missing checkouts first, then least recently used. Never the base, the current context or one in use |

```text
$ katalogr context add --commit HEAD~1
  c-bb5e1247735045b6 commit ledger @19714ed1e468 418013 bytes
```

### Linked worktrees

A linked git worktree has its own context, found through the main checkout's marker. katalogr reads
and writes that worktree's context, so edits in one worktree do not appear in another, and the git
hooks of each queue into their own context.

```text
$ git worktree add ../ledger-wt -b side
$ cd ../ledger-wt
$ katalogr status
index status; watermark 5, 1 pending (stale); context w-73544883aa69bdbe
$ katalogr find add
fn  ledger:src/totals.rs#fn:add  src/totals.rs:1
```

The new worktree reports a pending change for its own files, and `katalogr index` brings it
current. After removing the worktree, `context prune` clears its context.

See also: [Index lifecycle](index-lifecycle.md) for `index --since` and `audit`, and
[Connecting to kanbanr](../guide/kanbanr.md) for `impact --item`.

---

What changed in each version is in the [release notes](../releases/index.md).
