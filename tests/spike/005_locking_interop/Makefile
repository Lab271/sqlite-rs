# Locking-protocol interop spike (issue #8) — throwaway two-process
# harness; verification is the PASS/FAIL lines `run` prints for each of
# the five experiments (see findings.md for interpretation).

.DEFAULT_GOAL := help

.PHONY: help run

help: ## Show this help
	@echo ""
	@awk 'BEGIN {FS = ":.*?## "} \
	  /^# === .* ===$$/  { sub(/^# === /, ""); sub(/ ===$$/, ""); printf "\n\033[33m%s\033[0m\n", $$0 } \
	  /^[a-zA-Z0-9_-]+:.*?## / { printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2 }' \
	  $(MAKEFILE_LIST)
	@echo ""

# === Spike ===

run: ## Run all five locking-interop experiments against a live sqlite3
	cargo run --quiet
