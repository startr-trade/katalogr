# Navigation

**Questions answered:** where is this defined, what does it look like, what refers to it, who calls
it, which tests cover it, how are two things connected, and why does this line exist.

These commands read the index and never change your code (except `replace`). The examples use the
`ledger` project from [Getting started](../guide/getting-started.md), with its board repository
from [Connecting to kanbanr](../guide/kanbanr.md). Anywhere a command takes a symbol, you can give
a plain name (`record`), a canonical name (`ledger:src/lib.rs#fn:record`) or, for `find`, part of a
name. A plain name that matches more than one symbol is an error (exit 3) that lists the
candidates; give the canonical name instead.

Every command accepts the [global options](../guide/index.md#output-options-that-apply-everywhere).
Each answer ends with a `next:` line of follow-up commands; with `--json` the same hints are in
the `next` field.

## search

Rank symbols by BM25 over names, docs, section titles and contract keys. Identifiers are split into
words, so `record payment` finds `record`, `records_a_payment` and `Payment`.

```text
katalogr search <TERMS>... [--limit N] [--neutral] [--diagnostic]
```

```text
$ katalogr search record payment --limit 3
1.946220  fn  ledger:src/lib.rs#fn:record  src/lib.rs:13  [record@name record@doc record@contract payment@doc]
1.788401  section  board:…/features-spec/FEAT-7.md#section:record-payments  …
1.766438  fn  ledger:src/lib.rs#mod:tests/fn:records_a_payment  src/lib.rs:28  [record*@name …]
partial: capped at 3 (limit); absence is not proof
```

Columns: score, kind, canonical name, file and line, then in brackets which words matched where
(`@name`, `@doc`, `@contract`; `*` is a prefix match). `partial` means more hits exist than were
listed. `--limit` defaults to 20. By default the ranking favours files you are working on
(uncommitted, recently edited, on your branch); `--neutral` orders by relevance alone.
`--diagnostic` adds a trace on standard error. Exit 4 when nothing matches.

## find

List symbols whose name matches the text: a canonical name, or part of a plain name.

```text
$ katalogr find Payment
section  board:…/features-spec/FEAT-7.md#section:record-payments  …/FEAT-7.md:1
fn  ledger:src/lib.rs#mod:tests/fn:records_a_payment  src/lib.rs:28
struct  ledger:src/lib.rs#struct:Payment  src/lib.rs:7
fn  ledger:web/client.ts#fn:sendPayment  web/client.ts:7
interface  ledger:web/client.ts#interface:Payment  web/client.ts:1
```

Note that two symbols are called `Payment`, a Rust struct and a TypeScript interface. `find` lists
both; a command that needs exactly one would answer `ambiguous: 2 candidates` (exit 3).

## show

Print a symbol's lines, with the file and line range first. `--signature` prints only the signature
and the doc.

```text
$ katalogr show record
src/lib.rs:13-18
/// Record a payment and return its id.
// FEAT-7: payments must never be lost
pub fn record(p: &Payment) -> u64 {
    totals::add(p.cents);
    p.id
}

$ katalogr show record --signature
src/lib.rs:13-18
pub fn record(p: &Payment) -> u64
Record a payment and return its id.
```

## explain

Describe a symbol: signature, doc, its links in both directions and the items that explain it.
`--per-side <N>` is how many links to list on each side (default 10).

```text
$ katalogr explain record
fn  ledger:src/lib.rs#fn:record  src/lib.rs:13
signature: pub fn record(p: &Payment) -> u64
doc: Record a payment and return its id.
outgoing (2):
  annotated-by  board:…#item:FEAT-7  [Extracted]
  calls  ledger:src/totals.rs#fn:add  [Extracted]
incoming (1):
  calls  ledger:src/lib.rs#mod:tests/fn:records_a_payment  [Extracted]
items: board:…#item:FEAT-7
```

The bracket is the link's confidence: `Extracted` (read from the source), `Inferred` (by a rule), or
a mark that the reference resolves to several targets.

## refs

List what references a symbol, including through re-exports. A name with 50 or more targets is
folded into one line; `--expand` lists them all. `--diagnostic` explains the answer.

```text
$ katalogr refs add
calls  ledger:src/lib.rs#fn:record  src/lib.rs:13  [Extracted]
calls  ledger:src/totals.rs#fn:report  src/totals.rs:6  [Extracted]
```

Each line is the kind of link, the referrer, its location and its confidence.

## callers

List the callers of a symbol, `--depth <N>` call hops deep (default 1). Each line shows the depth at
which the caller was found.

```text
$ katalogr callers add --depth 2
fn  ledger:src/lib.rs#fn:record  src/lib.rs:13  [Extracted]  depth 1
fn  ledger:src/totals.rs#fn:report  src/totals.rs:6  [Extracted]  depth 1
fn  ledger:src/lib.rs#mod:tests/fn:records_a_payment  src/lib.rs:28  [Extracted]  depth 2
```

A call written inside a macro's arguments, such as `assert_eq!(record(&p), 1)`, was not linked in
our test project. Call the function on its own line (`let id = record(&p);`) when you want the call
to show up.

## tests

List the tests that exercise or verify a symbol, following calls up to `--depth <N>` hops (default
3). `--cases` lists only test cases, leaving out the suites that contain them. A test that reaches
the symbol through another function is marked `through`.

```text
$ katalogr tests add --cases
case  ledger:src/lib.rs#mod:tests/fn:records_a_payment  depth 2  extracted  1.0000  through ledger:src/lib.rs#fn:record
```

An empty answer (exit 0, no lines) means no test was found within the depth. For tests tied to board
requirements, see [Connecting to kanbanr](../guide/kanbanr.md#tests-that-verify-requirements).

## path

Find the shortest chain of links between two symbols. `--max-len <N>` is the longest chain to look
for (default 6).

```text
$ katalogr path record report
ledger:src/lib.rs#fn:record -calls-> ledger:src/totals.rs#fn:add  [Extracted]
ledger:src/totals.rs#fn:add <-calls- ledger:src/totals.rs#fn:report  [Extracted]
```

Links are followed in either direction; an arrow `<-` shows a step against the link's direction. No
chain is exit 4, with a note that absence is not proof:

```text
$ katalogr path record sendPayment
katalogr: no path from ledger:src/lib.rs#fn:record to ledger:web/client.ts#fn:sendPayment within 6 steps; absence is not proof (searched up to 6 links)
```

## why

Show the symbol at a line, who uses it and what explains it. The argument is `FILE:LINE` with FILE
relative to the repository root. `--no-use` leaves out callers, tests and contracts, which is faster.

```text
$ katalogr why src/lib.rs:17 --no-use
fn  ledger:src/lib.rs#fn:record  src/lib.rs:13
  commit  19714ed1e468  feat: first ledger
  …
```

Sections, in order: who uses it (`caller`, `test`, `contract`), notes (`NOTE`, `WHY` and
`RATIONALE` comments), the commit that introduced the line, the board items linked to it
(`annotated-by`, `changed-by`) and the goals they serve. The full output and the board side are on
[Connecting to kanbanr](../guide/kanbanr.md#2-reference-items-from-code). When two symbols tie for
the line, the answer is ambiguous (exit 3).

## map

Outline the project: symbols ranked by PageRank over the reference graph, most central first. The
first column is the rank score; `--neutral` ignores the working set.

```text
$ katalogr map
   4  item  board:…#item:FEAT-7  projects/ledger/…/FEAT-7.yaml:1
   3  req  board:…#item:FEAT-7/req:R-1  …
   2  fn  ledger:src/totals.rs#fn:add  src/totals.rs:1
   1  fn  ledger:src/lib.rs#fn:record  src/lib.rs:13
   …
```

This is the same map that `session-start` gives an agent (see
[Using katalogr with Claude Code](../guide/claude-code.md)). Use `--budget <TOKENS>` to cut it.

## replace

Replace a symbol's lines with the content of a file, then reindex. This is the one command here
that changes your source. The file must hold exactly the new text for the symbol's lines.

```text
$ katalogr replace add --file new_add.rs
replaced src/totals.rs:1-4 with src/totals.rs:1-3
```

The command prints the old and new line ranges. Review the change with `git diff`.

## Exit codes

| Code | When |
|---|---|
| 0 | An answer, including an empty one for `tests`, `refs` and `callers` of a symbol that exists |
| 3 | The name is ambiguous |
| 2 | A bad argument, such as a position that is not 40 hex digits |
| 4 | Nothing matched (`find`, `show`, `search`, `tests` of an unknown name), or no path was found |
| 6 | `--at` names a position that no context holds |

See also: [Impact](impact.md) for what a change reaches, and [Views and diagrams](views-and-diagrams.md)
to draw callers and paths.

---

What changed in each version is in the [release notes](../releases/index.md).
