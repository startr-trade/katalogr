# Lineage

**Questions answered:** what is this type, schema element or contract; where is it declared; what
are its fields; what binds to it; and where does the answer stop?

```text
katalogr lineage <TEXT> [--depth N] [--limit N] [--per-group N] [--expand]
katalogr mirrors [--kind KIND]...
```

## lineage

`TEXT` is the canonical or plain name of a symbol, or a contract name. A contract is a name that two
sides agree on, such as a route, a topic or a schema type; it is how code in different languages
meets.

```text
$ katalogr lineage ledger:src/lib.rs#struct:Payment
lineage of ledger:src/lib.rs#struct:Payment (struct); project ledger
fields:
  field:cents  integer
  field:id  integer
stops:
  no-code
  no-contract
```

The groups, when they have content, are the declaration, the fields, related types, what binds to
the thing, the code that uses it and the contracts it takes part in. Each group lists up to
`--per-group` hits (default 10) and the whole answer up to `--limit` (default 200); `--expand`
lists every hit up to the limit with no folding. `--depth` is how many contract hops to follow
(default 3).

**`stops:`** says where the answer ends and why. Here the struct has no code bound to it and no
contract, so there is nothing further to follow. It is how lineage tells "nothing there" from "I
did not look".

A plain name that matches two symbols is ambiguous (exit 3), as it is for `Payment` in `ledger`,
which is both a Rust struct and a TypeScript interface:

```text
$ katalogr lineage Payment
katalogr: ambiguous: 2 candidates
struct  ledger:src/lib.rs#struct:Payment  src/lib.rs:7
interface  ledger:web/client.ts#interface:Payment  web/client.ts:1
```

The web view has a type-lineage page that shows the same data; see
[Web view and exports](web-and-exports.md).

## mirrors

Compare mirrored copies of a structure: contracts that two or more places declare. It reports a
field that is missing on one side or that differs in type, in whether it is required, or in whether
it is many. Use `--kind` (repeatable, for example `schema_type`) to restrict it to one kind of
contract.

```text
$ katalogr mirrors
```

No output and exit 0 means no mirrored contract differs. `mirrors` exits 1 when a difference is
found, so it can be a CI gate. In `ledger` the Rust and TypeScript `Payment` are separate
declarations that no extension binds together, so there is nothing for `mirrors` to compare.

Exit codes: 0 for an answer; 1 for `mirrors` when a mirrored field differs; 3 for an ambiguous name.

---

What changed in each version is in the [release notes](../releases/index.md).
