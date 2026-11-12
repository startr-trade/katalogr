# Structure and health

**Questions answered:** which parts are central, which belong together, which depend on each other
in a circle, which code nothing reaches, how complete is the index, which symbols have a recorded
reason, and which references name nothing.

These commands read the whole graph, so on a small project like `ledger` most of them answer with a
few lines. Their results are stored by position and build, so asking again at the same position is
instant (see [stored answers](#stored-answers-answers)). Use `--fresh` to compute again.

Most of them take `--level symbol|file|package` to group the nodes, and `--strict` to leave out
weak links (inferred, or resolved to several targets).

## hubs

List the nodes with the highest weighted degree (links in plus links out): the places where many
things meet, and therefore the costly ones to change.

```text
katalogr hubs [--level LEVEL] [--min-degree N] [--limit N] [--strict]
```

```text
$ katalogr hubs --min-degree 1 --limit 5
symbol level: 30 nodes, mean degree 1.43
    2.00  in 1.00  out 1.00  fn  ledger:src/audit.rs#fn:seen  src/audit.rs:3
    2.00  in 2.00  out -0.00  fn  ledger:src/totals.rs#fn:add  src/totals.rs:1
    …
partial: capped at 5 (limit); absence is not proof
```

The header gives the number of nodes at that level and the mean degree. Each row is the weighted
degree, the weight in and out, and the node. The default level is `symbol` and `--limit` is 20.
Without `--min-degree`, the cut-off comes from the project's `insights.hub_min_degree` setting;
on a small project nothing reaches it and only the header prints.

## communities

Find groups of nodes that use each other more than the rest.

```text
katalogr communities [--level LEVEL] [--seed N] [--resolution GAMMA] [--members N] [--limit N]
                     [--of NAME] [--strict]
```

```text
$ katalogr communities --level symbol
modularity 0.5000  2 communities  23 unclustered
#1  4 members  cohesion 1.000  src  top: ledger:src/rules.rs#fn:limit, ledger:src/audit.rs#fn:seen, …
#2  3 members  cohesion 0.667  -  top: ledger:src/lib.rs#fn:record, …
```

`modularity` measures how well the groups separate (higher is better). `cohesion` is how
tightly a group holds together. The word after it is a label for the group (`src` here, `-` when it has none). `--resolution` raises or lowers the group size (higher gives smaller groups),
`--members` is how many members to list per group (default 5), and `--of <NAME>` shows only the
community of one node. The same `--seed` gives the same communities. The default level is `file`.

## cycles

List dependency cycles, each with one witness. It exits 1 when any exist, so it works as a CI gate.

```text
katalogr cycles [--level LEVEL] [--include-inferred] [--members N] [--limit N]
```

```text
$ katalogr cycles
1 cycles, 2 file level nodes in cycles
#1  size 2  [Extracted]  ledger:src/audit.rs -calls-> ledger:src/rules.rs -calls-> ledger:src/audit.rs
$ echo $?
1
```

`src/rules.rs` and `src/audit.rs` call each other, so they form a cycle at file level. At symbol
level the same code has none, because the calls go between different functions
(`cycles --level symbol`: `0 cycles`). Choose the level that matches the rule you want to enforce.
`--include-inferred` also follows inferred links; each cycle then shows its weakest confidence.

## dead-code

List candidates: code that no entry point reaches.

```text
katalogr dead-code [--internal] [--min-confidence high|medium|low] [--path PREFIX] [--limit N] [--check]
```

```text
$ katalogr dead-code
12 entry points; 1 high, 0 medium, 0 low
high  unreferenced  fn  ledger:src/audit.rs#fn:orphan  src/audit.rs:12  in 0
```

`high` means no link of any confidence reaches the code. It does not mean the code is certainly
dead: a registry keyed by strings, which no profile models, stays invisible, so check before you
delete. `medium` (`unreachable`) means something links to it, but nothing reachable does.

By default, public API items count as entry points, so `pub fn record` is not listed. `--internal`
does not count them, which shows what only the public surface keeps alive:

```text
$ katalogr dead-code --internal
1 entry points; 8 high, 4 medium, 0 low
high  unreferenced  fn  ledger:src/audit.rs#fn:verify  src/audit.rs:8  in 0
…
medium  unreachable  fn  ledger:src/totals.rs#fn:add  src/totals.rs:1  in 2
```

`--path` restricts the list to a repository-relative prefix. `--check` exits 1 when any candidate is
listed at the chosen confidence, for a CI gate (the default minimum is `medium`).

## coverage

Report how complete the index is: how many references resolved, were inferred or are unresolved,
dangling contracts, parse errors, and files no extension claims.

```text
$ katalogr coverage --repo ledger
coverage  files 8 indexed, 0 parse errors (0.0%), 0 unsupported, 0 failed, 0 unclaimed
refs      15: 80.0% resolved, 0.0% inferred, 20.0% unresolved; 1 dangling contracts
by repo       ledger  files 8  resolved 80.0% inferred 0.0% unresolved 20.0%  parse errors 0.0%
by extension  rust  …
by language   typescript  files 1  resolved 0.0% inferred 0.0% unresolved 100.0%  …
group         ledger/rust/rust  …
unclaimed     ledger  0 files:
```

Read it before you trust an absence: a language with a high unresolved share, or a repository with
many unclaimed files (`--suffixes <N>` lists the most common suffixes of those, default 20), is a
place where "no result" means less. `--repo` limits it to one repository of the project.

## rationale

List what explains a symbol, a path or the top hubs: comment markers (`NOTE:`, `WHY:`,
`RATIONALE:` in upper case), ADR citations, board items and documents.

```text
katalogr rationale [SYMBOL] [--path PREFIX] [--hubs N] [--limit N]
```

```text
$ katalogr rationale --hubs 3
item  ledger:src/lib.rs#fn:record  src/lib.rs:14  item:FEAT-7
no rationale: ledger:src/totals.rs#fn:add
no rationale: ledger:src/totals.rs#fn:report
```

`--hubs <N>` takes the top N hubs and names those with no rationale; they are the central places
that nobody explained. For a single line, `katalogr why` gives the same facts with the commit and
goals ([Connecting to kanbanr](../guide/kanbanr.md)).

## dangling

List references that name something that does not exist. It exits 1 when any dangle.

```text
katalogr dangling [--kind KIND]...
```

```text
$ katalogr dangling
annotated-by  ledger:src/lib.rs#fn:unused_helper  src/lib.rs:21  wants board-item:FEAT-99
$ echo $?
1
```

Each line is the link kind, the referrer, its location and the key it wants. By default it checks
`links-to` (a Markdown link to a missing file or section), `annotated-by` (a board item that does
not exist) and `refs-item` (a commit trailer for an item that does not exist). `--kind` is
repeatable and also takes other kinds, such as `verified-by` for test names on requirements, and
`contract` for contracts with only one side.

## Stored answers (`answers`)

The answers of `hubs`, `communities`, `cycles`, `dead-code`, `map` and `coverage` are stored under
the project, keyed by position and build. A second ask is a lookup. Three commands manage them:

```text
$ katalogr answers stats
7 stored answers, 24086 bytes (limit 256 MiB)
  communities: 1
  coverage: 1
  …
$ katalogr answers check
checked 6: 6 match, 0 differ
skipped: 1 at another position, 0 under another build or index, 0 unknown
```

- `answers check` recomputes each stored answer of the current position and build and compares.
  Exit 0 when all match, 1 when one differs, 5 when the position cannot be held. `--limit <N>`
  limits how many are checked. `--repair` replaces a differing stored answer with the fresh one.
- `answers stats` counts the stored answers and their bytes by command.
- `answers prune --max-mib <N>` removes the oldest until at most that size remains.

Exit codes in this group: 1 for `cycles` (any cycle), `dead-code --check` (any candidate),
`dangling` (any dangling reference) and `answers check` (a difference); 0 otherwise.

---

What changed in each version is in the [release notes](../releases/index.md).
