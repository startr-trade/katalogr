# Workspaces

**Questions answered:** which other projects does this one connect to, are they readable here, and
who consumes what another provides?

A **workspace** is a project plus its **peers**: other projects whose contracts it joins. A peer is
a separate project, often in a separate kindex and owned by another team. katalogr reads the peer's
index (never its code), so a contract that one project provides and another consumes can be followed
across the boundary. See [Lineage](lineage.md) for contracts.

## Name a peer

```text
katalogr workspace add <PROJECT> [--alias ALIAS]
katalogr workspace locate <ALIAS> <DIR>
```

`workspace add` names a peer in this project's settings (`workspace.peers` in `config.yaml`). That
file is committed to the kindex, so it holds **no path**. A peer in another kindex is named through an
**alias**; `workspace locate` records, on this machine only, which folder the alias points to.

```text
$ katalogr workspace add billing --alias payments
peer payments/billing added
$ katalogr workspace status
payments/billing  no-location  fix: katalogr workspace locate payments <dir>
$ katalogr workspace locate payments …/kx3
kindex payments located on this machine
$ katalogr workspace status
payments/billing  ok  billing@8d0dae418b0f
```

Without `--alias`, the peer is a project in this same kindex.

## workspace status

States each peer can be in. A peer that cannot be read is a state, not an error, and each state with
a fix names the command:

| State | Meaning |
|---|---|
| `ok` | Read in full. Shows `<project>@<commit>` |
| `no-location` | The alias is not located on this machine. Run `workspace locate` |
| `not-found` | The kindex has no such project |
| `newer-format` | The peer's index was written by a newer katalogr |
| `no-index` | The peer has no index here; clone its repos and index it |
| `unavailable` | The peer's index could not be opened (another schema, or corrupt) |
| `timeout` | The peer did not answer in time |

A peer is read from its **base** context only, and it is stale when it has pending changes.

## workspace map

Which project consumes what another provides, over the ends you can read.

```text
$ katalogr workspace map
projects: billing, ledger
```

When contracts connect the projects, the map shows which project consumes what another provides.
A peer you cannot read appears under `peers:` with its state, and the map is marked partial:

```text
katalogr: warning: peer billing: not read (not-found)
projects: billing, ledger
partial: capped at 1 (peers; 1 found); absence is not proof
peers:
  billing: not-found
```

## What peers change

When peers are named, these commands also follow contracts into them: `refs`, `callers`,
`dangling`, `impact`, and the `contracts` and `impact` [views](views-and-diagrams.md). A reference
that is dangling here but provided by a peer is resolved. One hop only: `callers` does not walk on
inside the peer.

- A peer's nodes are named `<project>//<canonical>` so they cannot collide with yours.
- An end you cannot read is shown as one **opaque** node per project (`<project>//hidden`). The
  answer is then partial, because something unreadable might also connect.
- An answer that depends on a peer that is not `ok`, is stale or lacks a repository is not **exact**.
  Its view ID is not stored, and `view snapshot` refuses it (exit 5).
- Symbol search (`find`, `search`) reads this project only; peers join through contracts.

`--no-peers` reads this project alone, for any command:

```text
$ katalogr find add --no-peers
```

Use it to check whether an answer changed because of a peer.

## Exit codes

`workspace status` and `workspace map` exit 0 even when a peer is unreadable; the state is in the
output. A command that cannot be exact because of a peer says so in the answer and with a
warning that names the project and the fix, never a path.

---

What changed in each version is in the [release notes](../releases/index.md).
