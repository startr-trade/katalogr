# Retrieval evaluation

**Question answered:** does `search` still find the right things after a change to the ranking, the
profiles or the code?

`katalogr eval` runs **golden sets**: queries with the answers a person judged right. It prints
standard retrieval measures, compares them with a **baseline**, and can fail a build when they
regress.

```text
katalogr eval run   [--sets PATH]... [--k LIST] [--param KEY=V]... [--baseline FILE] [--gate]
                    [--write-baseline FILE] [--project-sets]
katalogr eval sweep --param KEY=V1,V2,... [--sets PATH]... [--k LIST] [--objective METRIC]
```

## A golden set

A file `golden.yaml` holds the format version, a name, a version number and the queries. Each query
has an id, an operation (`search`, `map`, `refs` or `explain`), an input and the evidence: the
symbols that should come back, graded 3 (the answer itself), 2 (a direct useful neighbour) or 1
(related context).

```yaml
format: 1
set: ledger-basic
version: 1
repos:
  - {name: ledger, path: repos/ledger}
queries:
- id: q1
  op: search
  input: record payment
  evidence:
  - {name: "ledger:src/lib.rs#fn:record", grade: 3}
- id: q2
  op: search
  input: running total
  evidence:
  - {name: "ledger:src/totals.rs#fn:add", grade: 3}
```

In **fixture mode** (the default), `repos` names the repositories, as paths relative to the file.
katalogr indexes them into a private temporary project, runs the queries and throws the project
away. Use it for synthetic corpora in a repository. A query can also set `route`
(`found`, `ambiguous` or `no-match`), a `scope` (`repos`, `paths`) and `tags`. Change a set's
`version` whenever its queries, grades or corpus change.

In **project mode** (`--project-sets`), sets are read from the kindex, at `projects/<name>/eval/*.yaml`,
and run against the live index. A set there must not declare `repos`. Keep sets about real
repositories in the kindex, not in a public repository.

## eval run

```text
$ katalogr eval run --sets golden/golden.yaml --k 1,5
set  op  scope  hit@1  hit@5  mrr  ndcg@5  tok/rel  n
*  *  *  1.000  1.000  1.000  1.000  320.000  2
*  *  within  1.000  1.000  1.000  1.000  320.000  2
ledger-basic  *  *  1.000  1.000  1.000  1.000  320.000  2
ledger-basic  search  *  1.000  1.000  1.000  1.000  320.000  2
```

Rows are the overall result (`*`), then per set and operation, and split by scope: `within` (the
evidence is in one repository), `cross` (two or more) and `none`. Columns:

| Column | Meaning |
|---|---|
| `hit@k` | The share of queries with any right answer in the first k results |
| `mrr` | Mean reciprocal rank of the first right answer |
| `ndcg@k` | Rank-weighted gain over the first k, using the grades (1.0 is ideal) |
| `tok/rel` | Context efficiency: tokens spent per relevant hit (lower is better) |
| `n` | Queries counted |

`--k <LIST>` sets the cut-offs (comma separated, 1 to 50; default 1, 5 and 10). `--param KEY=V`
overrides one ranking parameter for a what-if run. `--sets <PATH>` is a file or folder and is
repeatable.

## Baselines and gates

Write a baseline, then compare against it:

```text
$ katalogr eval run --sets golden/golden.yaml --k 1,5 --write-baseline golden/baseline.json
…
$ katalogr eval run --sets golden/golden.yaml --k 1,5 --baseline golden/baseline.json --gate
…
gate passed: 0 regressions, 0 improvements, 36 unchanged
$ katalogr eval run --sets golden/golden.yaml --gate
katalogr: --gate needs a baseline (--baseline FILE)
```

`--write-baseline <FILE>` writes the report and exits 0. `--baseline <FILE>` prints how each measure
moved. `--gate` exits 1 when a measure regressed beyond tolerance; it needs `--baseline`, or exits 2.
Run it in CI so a change to ranking cannot quietly make search worse. In project mode the baseline
defaults to `eval/baseline.json` in the project, when that file exists.

Use the same `--k` list for the baseline and the comparison. A measure that improved is listed as
`improved <name> <old> -> <new>`.

## eval sweep

Evaluate every combination of parameter ranges and rank them by an objective metric, to choose
settings by measurement rather than by guess.

```text
$ katalogr eval sweep --sets golden/golden.yaml --param search.k1=0.9,1.2 --objective retrieval.ndcg@5 --k 1,5
retrieval.ndcg@5 (Higher is better)
1.000000  search.k1=0.9
1.000000  (defaults)  [default]
```

Each `--param` takes a key and a comma-separated list of values; the combinations multiply, and at
most 256 are run. An unknown key is exit 2 and the error lists the valid keys, among them
`search.k1`, `search.b`, `search.min_prefix` and the field weights `search.weight.name`,
`search.weight.doc` and `search.weight.section`. The default objective is `retrieval.ndcg@10`. The
`(defaults)` row is the unchanged setting, so you can see whether any combination beats it.

Exit codes: 0 on success; 1 when `--gate` finds a regression; 2 for a bad golden file, an unknown
parameter or `--gate` without a baseline.

---

What changed in each version is in the [release notes](../releases/index.md).
