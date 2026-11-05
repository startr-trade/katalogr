<!-- Thanks for contributing to katalogr! Keep PRs focused. Issues are off: describe the problem here. -->

## What & why

<!-- What does this change, and what problem does it solve? -->

## How verified

<!-- Commands you ran and what you observed. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] The retrieval gate: `cargo run --locked -p katalogr -- eval run --sets fixtures/eval --baseline fixtures/eval/baseline.json --gate`

## Checklist

- [ ] Every behaviour I added or changed has a test, and it passes.
- [ ] The core names no language, format or protocol (`cargo test -p katalogr-archtest` passes).
- [ ] Fixtures are synthetic: no real repository's names, paths or code, no local paths, no emails.
- [ ] No golden file or the eval baseline was regenerated to make a test pass.
- [ ] New dependencies are pure Rust and pinned exactly.
- [ ] Docs and the README are updated if user-facing behaviour changed.

## Notes for the reviewer

<!-- Anything you want a second look at. -->
