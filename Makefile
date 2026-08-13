# sqlite-rs

.DEFAULT_GOAL := help

.PHONY: help test-spikes assurance assurance-gate traceability coverage

help: ## Show this help
	@echo ""
	@awk 'BEGIN {FS = ":.*?## "} \
	  /^# === .* ===$$/  { sub(/^# === /, ""); sub(/ ===$$/, ""); printf "\n\033[33m%s\033[0m\n", $$0 } \
	  /^[a-zA-Z0-9_-]+:.*?## / { printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2 }' \
	  $(MAKEFILE_LIST)
	@echo ""

# === Assurance ===

assurance: ## Assurance dashboard: spec -> code -> test traceability + evidence (VERBOSE=true for per-requirement detail)
	@python3 tools/assurance.py $(if $(VERBOSE),--verbose)

assurance-gate: ## CI gate: fail if completeness or scenario-weighted coverage is below 75%
	@python3 tools/assurance.py --min 0.75

traceability: ## Fast path: traceability only, no corpus/coverage I/O
	@python3 tools/assurance.py --traceability-only $(if $(VERBOSE),--verbose)

coverage: ## Cache line coverage for the assurance Evidence level (cargo-llvm-cov)
	cargo llvm-cov --json --output-path target/llvm-cov.json

# === Spikes ===

test-spikes: ## Run all parser-spike variants (tests/spike/001_parser)
	$(MAKE) -C tests/spike/001_parser test
