# Dependencies

**Question answered:** which package versions do my lockfiles pin, which members pin them, and
which code uses them?

```text
katalogr deps [--package NAME] [--limit N] [--used-by-limit N]
```

katalogr reads lockfiles (Cargo's `Cargo.lock` in these examples) and links each pinned version to
the member that pins it and the manifests and code that use it.

```text
$ katalogr deps
serde 1.0.200  cargo  ledger:Cargo.lock  registry+https://github.com/rust-lang/crates.io-index
    pinned by ledger:Cargo.lock#member:ledger
    used by ledger:Cargo.toml#crate:ledger (Cargo.toml:1)
```

Each package version is one block. The first line is the package, version, ecosystem, the lockfile
that pins it and its source. `pinned by` lists the members (workspace crates or packages) that
resolve to this version; `used by` lists up to `--used-by-limit` users (default 20), with the
location of the dependency declaration.

| Flag | Meaning |
|---|---|
| `--package <NAME>` | Only this package name |
| `--limit <N>` | How many package versions to list (default 1000) |
| `--used-by-limit <N>` | How many users to list per version (default 20) |

When the same package is pinned at two versions, both appear, each with its own members. That is
how you spot a version split. With `--json` the answer carries the same data for scripts.

## In the web view

The web view has a Dependencies page, built over the same `deps` answer. It shows the packages table
and needs the `deps` capability, which the binary reports in `katalogr contract`. A panel for known
vulnerabilities is not built yet and says so on the page. Start the web view with
`katalogr serve`; see [Web view and exports](web-and-exports.md).

Exit codes: 0 for an answer. A `--package` that matches nothing prints nothing and exits 0.

---

What changed in each version is in the [release notes](../releases/index.md).
