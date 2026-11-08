# katalogr: run what CI runs, before pushing.
#
#   make          list the targets
#   make ci       everything .github/workflows/ci.yml runs, cheapest first, plus the Windows and
#                 macOS compile checks (clippy over every test for both targets)
#
# The toolchain is whatever rust-toolchain.toml pins (the same version CI installs), so a clippy
# lint that CI would raise is raised here too. `make cross` needs the targets installed:
#   rustup target add x86_64-pc-windows-msvc aarch64-apple-darwin
#
# Heavy targets run their cargo under the shared build lock and a memory cap when `flock` and
# `systemd-run` exist, and plain otherwise. Override with `make ci HEAVY=` to skip both.

SHELL := /bin/bash
.DEFAULT_GOAL := help
.NOTPARALLEL:

CARGO_JOBS ?= 4
LOCK ?= $(HOME)/.cache/heavy-build.lock
MEM ?= 14G
TARGETS := x86_64-pc-windows-msvc aarch64-apple-darwin x86_64-apple-darwin

HEAVY ?= $(strip \
	$(if $(shell command -v flock 2>/dev/null),flock $(LOCK)) \
	$(if $(shell systemd-run --user --scope -q true 2>/dev/null && echo ok),systemd-run --user --scope -q -p MemoryMax=$(MEM)))

.PHONY: help ci fmt lint cross test failpoints eval no-c

help: ## List the targets.
	@grep -hE '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  make %-10s %s\n", $$1, $$2}'

ci: ## Everything CI runs on Linux plus the cross-target checks, cheapest first (no-c only on a dependency change).
	@$(MAKE) --no-print-directory fmt
	@$(MAKE) --no-print-directory lint
	@$(MAKE) --no-print-directory cross
	@$(MAKE) --no-print-directory test
	@$(MAKE) --no-print-directory failpoints
	@$(MAKE) --no-print-directory eval
	@if [ -n "$(NO_C_CHANGED)" ]; then $(MAKE) --no-print-directory no-c; \
	 else echo "no-c: skipped (no dependency or toolchain change since $(BASE); run make no-c to force)"; fi
	@echo "make ci: all steps passed"

fmt: ## cargo fmt --check.
	cargo fmt --all -- --check

lint: ## Both clippy runs from ci.yml (workspace; katalogr without default features).
	$(HEAVY) cargo clippy --workspace --all-targets --locked -j$(CARGO_JOBS) -- -D warnings
	$(HEAVY) cargo clippy -p katalogr --no-default-features --all-targets --locked -j$(CARGO_JOBS) -- -D warnings

cross: ## Clippy over every test for Windows and macOS (compiles, does not link or run).
	@for t in $(TARGETS); do \
		echo "== $$t"; \
		$(HEAVY) cargo clippy --workspace --all-targets --locked -j$(CARGO_JOBS) --target $$t -- -D warnings || exit 1; \
		$(HEAVY) cargo clippy -p katalogr --no-default-features --all-targets --locked -j$(CARGO_JOBS) --target $$t -- -D warnings || exit 1; \
	done

# cargo-nextest, when installed, runs the test binaries side by side instead of one after another
# (same tests; doctests still run through cargo test). Without it, plain cargo test.
NEXTEST := $(shell cargo nextest --version >/dev/null 2>&1 && echo yes)
TEST_THREADS ?= 6

test: ## Workspace tests (in parallel with cargo-nextest when installed).
ifeq ($(NEXTEST),yes)
	$(HEAVY) cargo nextest run --workspace --locked --build-jobs $(CARGO_JOBS) --test-threads $(TEST_THREADS) --no-fail-fast
	$(HEAVY) cargo test --workspace --doc --locked -j$(CARGO_JOBS)
else
	$(HEAVY) cargo test --workspace --locked -j$(CARGO_JOBS) --no-fail-fast -- --test-threads=4
endif

failpoints: ## The engine crash suite.
	$(HEAVY) cargo test -p katalogr-engine --features failpoints --locked -j$(CARGO_JOBS) -- --test-threads=4

eval: ## The retrieval gate.
	$(HEAVY) cargo run --locked -j$(CARGO_JOBS) -p katalogr -- eval run --sets fixtures/eval --baseline fixtures/eval/baseline.json --gate

# The no-C proof only changes when dependencies or the toolchain do, so `make ci` runs it only
# when this branch changes those files against BASE (CI does the same, plus nightly and on tags).
BASE ?= origin/main
NO_C_PATHS := Cargo.lock ':(glob)**/Cargo.toml' rust-toolchain.toml .cargo
NO_C_CHANGED = $(shell { git diff --name-only $(BASE)...HEAD -- $(NO_C_PATHS); git diff --name-only HEAD -- $(NO_C_PATHS); } 2>/dev/null | head -1)

# Its own target directory: cargo does not rebuild for a changed CC, so reusing target/ would
# pass on artifacts a C compiler built. Everything in target/no-c was built without one.
no-c: ## Build everything, tests included, with no C compiler available (the build is the proof).
	$(HEAVY) env CC=false CXX=false CARGO_TARGET_DIR=target/no-c cargo build --workspace --all-targets --locked -j$(CARGO_JOBS)
