# Contributing

Thanks for your interest in katalogr. Please read the [Code of Conduct](CODE_OF_CONDUCT.md) first,
and report security problems privately as described in [SECURITY.md](SECURITY.md), never in a
public pull request.

Issues are turned off on this repository. Propose a change with a pull request, and keep it
focused. For a large change, describe the problem and your approach in the pull request first so a
maintainer can say early whether it fits.

## What a good change looks like

- State **why** the change exists, in the pull request description.
- Every behaviour you add or change has a **test**, and the test passes. A claim about speed or
  size names the test or benchmark that checks it.
- The core stays **neutral**: it names no specific language, file format or protocol. Support for
  one lives in its own extension crate. `cargo test -p katalogr-archtest` enforces this.
- Test fixtures are **synthetic**. Do not add code, names or paths from a real third-party
  repository, and no local absolute paths or email addresses.
- Dependencies are **pure Rust**, pinned exactly, and permissively licensed. katalogr must build
  with `cargo` alone, with no C toolchain.

## Building and checking

You need the Rust toolchain from `rust-toolchain.toml` (via rustup). Before opening a pull
request, run what CI runs:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy -p katalogr --no-default-features --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test -p katalogr-engine --features failpoints --locked
cargo run --locked -p katalogr -- eval run --sets fixtures/eval --baseline fixtures/eval/baseline.json --gate
```

Golden files and the evaluation baseline (`fixtures/eval/baseline.json`) record agreed behaviour.
Do not regenerate one to make a failing test pass; explain the difference in the pull request
instead.

## Pull requests

The maintainers in [CONTRIBUTORS](CONTRIBUTORS) review every pull request. Commits are small and
say what changed and why. By contributing you agree that your work is licensed under the
Apache License 2.0 or the MIT license, at the user's option, as the rest of the project is.
