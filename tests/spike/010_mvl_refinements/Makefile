# rust-refine spike (issue #371) — standalone crate, no ties to the main
# sqlite-rs package. See findings.md for the full narrative.

.DEFAULT_GOAL := help

.PHONY: help test build lint prove clean

help: ## Show this help
	@echo ""
	@awk 'BEGIN {FS = ":.*?## "} \
	  /^# === .* ===$$/  { sub(/^# === /, ""); sub(/ ===$$/, ""); printf "\n\033[33m%s\033[0m\n", $$0 } \
	  /^[a-zA-Z0-9_-]+:.*?## / { printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2 }' \
	  $(MAKEFILE_LIST)
	@echo ""

# === Spike ===

build: ## Compile the spike crate
	cargo build

test: ## Run the runtime-enforcement test suite (verifies the assert! injections, not the static-proof layers)
	cargo test

lint: ## fmt --check + clippy on the spike crate
	cargo fmt --check
	cargo clippy --all-targets

prove: ## Run cargo mvl prove against src/lib.rs and print each obligation's layer (requires cargo-mvl installed at this crate's mvl rev — see findings.md)
	cargo mvl prove src/lib.rs

clean: ## Remove build artifacts
	cargo clean
