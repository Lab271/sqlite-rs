# Parser toolchain spike (issue #1) — one standalone crate per variant,
# deliberately not a Cargo workspace so each toolchain's deps stay isolated.

.DEFAULT_GOAL := help

.PHONY: help test test-001_lemon-rs test-002_pomelo test-003_lalrpop test-004_pest

help: ## Show this help
	@echo ""
	@awk 'BEGIN {FS = ":.*?## "} \
	  /^# === .* ===$$/  { sub(/^# === /, ""); sub(/ ===$$/, ""); printf "\n\033[33m%s\033[0m\n", $$0 } \
	  /^[a-zA-Z0-9_-]+:.*?## / { printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2 }' \
	  $(MAKEFILE_LIST)
	@echo ""

# === Variants ===

test: test-001_lemon-rs test-002_pomelo test-003_lalrpop test-004_pest ## Run cargo test for every spike variant

test-001_lemon-rs: ## Run the lemon-rs (vendored Lemon LALR) variant
	cd 001_lemon-rs && cargo test

test-002_pomelo: ## Run the pomelo (proc-macro LALR) variant
	cd 002_pomelo && cargo test

test-003_lalrpop: ## Run the lalrpop (LR(1) DSL) variant
	cd 003_lalrpop && cargo test

test-004_pest: ## Run the pest (PEG) variant
	cd 004_pest && cargo test
