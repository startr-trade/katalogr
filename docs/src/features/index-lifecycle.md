# Index lifecycle

**Questions answered:** is the index current and healthy, how do I move it between machines, how do
I prove a rebuild gives the same graph, and what did the indexer read from one file?

## index and status

`katalogr index` queues changes and brings the index up to date. `katalogr status` shows the state.
See [Getting started](../guide/getting-started.md) for the day-to-day use.

```text
katalogr index [--full | --since REV | --range A..B] [--history] [--no-reports]
```

| Form | Does |
|---|---|
| `index` | Process the queued changes (and queue the checkout's changes) |
| `index --full` | Rescan every repository of the project |
| `index --since <REV>` | Index the changes since a revision, up to `HEAD` |
| `index --range <A..B>` | Index the changes in a revision range |
| `index --history` | Also index commits that carry item trailers (`Refs:`); with `--since`, only commits after the revision |
| `index --no-reports` | Do not build the diff reports that merges asked for |

```text
$ katalogr status
index status; watermark 22, 0 pending; context w-74e602dc4f11f0c9
```

Reading the line: the **watermark** counts processed changes; **pending** counts queued ones, and
`(stale)` follows when some are waiting. The context is the one the command reads. A first index
that is still queued is reported by `session-start` and by a warning (`no index yet`).

## doctor

Check the index and print a fix for each problem. It exits 1 on findings at or above `--fail-on`.

```text
katalogr doctor [--fail-on error|warning] [--skip-git] [--verbose]
```

```text
$ katalogr doctor
katalogr doctor: katalogr 0.1.1, shard format 2, cache schema 15
warning KD-GIT-001  1 files people write differ from HEAD in the kindex (0 modified, 1 added, 0 deleted)
        fix: git -C <kindex-dir> add projects/ledger && git -C <kindex-dir> commit -m "update project"
summary: 0 error, 1 warning, 0 info; 10 checks (1 with findings, 0 skipped)
```

Each finding has a severity (`error`, `warning`, `info`), a code (`KD-GIT-001`), a message and a
`fix:` line. `--fail-on error` lets warnings pass in CI. `--skip-git` leaves out the comparison of
the project files with the kindex's git repository. `--verbose` lists a few repository-relative keys
per finding. Exit 0 when nothing is at or above the level, 1 otherwise. Commit the project's files
in the kindex when `doctor` says they differ.

## snapshot: move an index

A snapshot is the graph in one file, to install an index without re-reading every file, for example
in CI or on a new machine.

```text
katalogr snapshot export --out <FILE>
katalogr snapshot import <FILE> [--replace] [--digest HEX]
katalogr snapshot show <FILE> [--verify]
```

```text
$ katalogr snapshot export --out ledger.snap
wrote …/ledger.snap: 1 repos, 14771 bytes in the file, digest 4b1673b1419f
$ katalogr snapshot show ledger.snap
format: 1
cache schema: 15
facts version: d87d1cd1d332…
database bytes: 151552
digest: 4b1673b1419f7783bb4bf65d242e2b7512bf1a501ed30286021ee9a780227f34
position ledger: 1514e9acba3e738fe9c87f197b5b885c7aaf2ae3
$ katalogr snapshot show ledger.snap --verify
…
payload verified
```

- **export** writes a text header (the positions, the facts version, the cache schema, the digest)
  then the compressed database. It refuses (exit 5) when the index is not current, when a repository
  is not at a clean commit, or when the file would be tracked by git. In our test, a project with a
  second repository (the board) that was not the marked one was refused with
  `repo board has no recorded position`; a project with one repository exported.
- **show** prints the manifest and needs no project, except with `--verify`, which also checks the
  payload against the digest.
- **import** installs the file as the project's base index, atomically, after checking the schema,
  the repositories and the digest. `--digest <HEX>` pins the digest you expect. It refuses (exit 5)
  when an index already exists, unless you pass `--replace`. A later read re-extracts only what
  differs from the snapshot's commits. A damaged file is exit 1. `--context` is not accepted.

```text
$ katalogr snapshot import ledger.snap
installed …/ledger.snap: 1 repos, digest 4b1673b1419f
```

A snapshot holds symbol names, paths and signature text. Keep it as private as the code. Do not
commit it.

## audit: prove a rebuild is the same

`audit record` writes a record of the graph's digest at the current positions, once per release.
`audit verify` later checks that a fresh index of the same code gives the same digest.

```text
$ katalogr audit record
recorded audit/a0d3e7e07404.json: facts d87d1cd1d332, graph a44fd943d809, 1 repos
$ katalogr audit verify
match
```

The record is a file `audit/<positions-hash>.json` in the project. Commit it in the kindex.
`audit verify [RECORD]` compares the index with a record (default: the stored record of the current
positions) and exits:

| Code | Meaning |
|---|---|
| 0 | Match |
| 1 | Mismatch |
| 4 | No record for these positions: `run katalogr audit record at a release` |
| 5 | The record is not comparable (another facts version) or the checkouts are not at its positions |

## shard show

Print what the index holds about one file, as the indexer would extract it. It reads the working
tree, or the file's blob at `--commit <REV>`, and writes nothing.

```text
$ katalogr shard show ledger:src/totals.rs
{
  "artifact": {
    "facts": { "package": { "Str": "ledger" }, "target": { "Str": "lib:ledger" } },
    "name": "ledger:src/totals.rs"
  },
  "exports": [ … ],
  …
```

The target is `<repo>:<path>`. Use it to see why a symbol or link is, or is not, in the index: the
fragment lists the symbols, references, exports and facts that file contributed.

## profiles

A **profile** is a set of rules that recognises connections, such as how a route in a handler meets
the client that calls it (HTTP in Rust and in TypeScript are built in).

```text
$ katalogr profiles list
builtin:generic.yaml
builtin:http-rust.yaml
builtin:http-ts.yaml
```

`profiles list` shows the built-in profiles, the project's own profile files, which built-ins they
replace and the files that are refused. `katalogr profiles show <NAME>` prints a built-in profile.
To change one, copy it into the project's `profiles/` folder in the kindex; a file with the same
`profile:` name replaces the built-in.

```text
$ katalogr profiles show http-ts > kx/projects/ledger/profiles/http-ts.yaml
```

Then `katalogr index --full` to apply it.

---

What changed in each version is in the [release notes](../releases/index.md).
