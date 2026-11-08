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
TARGETS := x86_64-pc-windows-msvc aarch64-apple-darwin

HEAVY ?= $(strip \
	$(if $(shell command -v flock 2>/dev/null),flock $(LOCK)) \
	$(if $(shell systemd-run --user --scope -q true 2>/dev/null && echo ok),systemd-run --user --scope -q -p MemoryMax=$(MEM)))

.PHONY: help ci fmt lint cross test failpoints eval no-c

help: ## List the targets.
	@grep -hE '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  make %-10s %s\n", $$1, $$2}'

ci: ## Everything CI runs on Linux plus the cross-target checks, cheapest first.
	@$(MAKE) --no-print-directory fmt
	@$(MAKE) --no-print-directory lint
	@$(MAKE) --no-print-directory cross
	@$(MAKE) --no-print-directory test
	@$(MAKE) --no-print-directory failpoints
	@$(MAKE) --no-print-directory eval
	@$(MAKE) --no-print-directory no-c
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

test: ## Workspace tests.
	$(HEAVY) cargo test --workspace --locked -j$(CARGO_JOBS) -- --test-threads=4

failpoints: ## The engine crash suite.
	$(HEAVY) cargo test -p katalogr-engine --features failpoints --locked -j$(CARGO_JOBS) -- --test-threads=4

eval: ## The retrieval gate.
	$(HEAVY) cargo run --locked -j$(CARGO_JOBS) -p katalogr -- eval run --sets fixtures/eval --baseline fixtures/eval/baseline.json --gate

no-c: ## Build and test with no C compiler available.
	$(HEAVY) env CC=false CXX=false cargo build --workspace --locked -j$(CARGO_JOBS)
	$(HEAVY) env CC=false CXX=false cargo test --workspace --locked -j$(CARGO_JOBS) -- --test-threads=4
