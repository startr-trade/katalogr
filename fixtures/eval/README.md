# Retrieval evaluation fixtures

Synthetic golden sets for `katalogr eval` (FEAT-061). Nothing here describes a real repository;
golden sets and baselines about real repositories live only in the project's kindex, under
`projects/<project>/eval/`.

- `core/golden.yaml`: one repository (`repos/app`: Rust, TypeScript, Markdown), 24 queries.
- `cross/golden.yaml`: two repositories (`repos/svc`, `repos/ui`) linked through a WASM export
  contract, 10 queries.
- `baseline.json`: the checked-in baseline the CI gate compares against.

## Golden set format 1

```yaml
format: 1
set: core                # [a-z0-9][a-z0-9-]*, unique in a run
version: 1               # integer >= 1; bump whenever queries, grades or the corpus change
repos:                   # fixture mode only: repositories relative to this directory
  - {name: app, path: repos/app}
queries:
  - id: q01              # unique in the set
    op: search           # search | map | refs | explain
    input: ledger balance   # required except for map
    route: found         # found (default) | ambiguous | no-match
    scope: {repos: [app], paths: ["src/**"]}   # optional
    tags: [phrase]
    evidence:            # required (at least one) when route is found, forbidden otherwise
      - {name: "app:src/ledger.rs#impl:Ledger/fn:balance", grade: 3}
```

Grades: 3 is the answer itself, 2 a direct useful neighbour (caller, documenting section,
implementing type), 1 related context worth seeing. Anything not graded is irrelevant. Take
evidence names from `katalogr find` on the indexed fixture; never invent one.

## Changing a set

Bump `version` when you change a set, then re-bless the baseline and review the diff:

    katalogr eval run --sets fixtures/eval --write-baseline fixtures/eval/baseline.json

CI runs `katalogr eval run --sets fixtures/eval --baseline fixtures/eval/baseline.json --gate`.
