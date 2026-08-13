# sqlite-rs

.DEFAULT_GOAL := help

.PHONY: help test test-spikes assurance assurance-gate traceability coverage spike-001 spike-002

help: ## Show this help
	@echo ""
	@awk 'BEGIN {FS = ":.*?## "} \
	  /^# === .* ===$$/  { sub(/^# === /, ""); sub(/ ===$$/, ""); printf "\n\033[33m%s\033[0m\n", $$0 } \
	  /^[a-zA-Z0-9_-]+:.*?## / { printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2 }' \
	  $(MAKEFILE_LIST)
	@echo ""

# === Test ===

test: ## Run the test suite
	cargo test

# === Assurance ===

assurance: ## Assurance dashboard: spec -> code -> test traceability + evidence (VERBOSE=true for per-requirement detail)
	@python3 tools/assurance.py $(if $(VERBOSE),--verbose)

assurance-gate: ## CI gate: fail if completeness or scenario-weighted coverage is below 75%
	@python3 tools/assurance.py --min 0.75

traceability: ## Fast path: traceability only, no corpus/coverage I/O
	@python3 tools/assurance.py --traceability-only $(if $(VERBOSE),--verbose)

coverage: ## Run the test suite under coverage instrumentation and print a line coverage report (cargo-llvm-cov)
	cargo llvm-cov --no-report
	cargo llvm-cov report
	cargo llvm-cov report --json --output-path target/llvm-cov.json

# === Spikes ===

spike-001: ## Run spike 001 — parser toolchain comparison (tests/spike/001_parser)
	$(MAKE) -C tests/spike/001_parser test

spike-002: ## Run spike 002 — file reading (tests/spike/002_file_reading)
	$(MAKE) -C tests/spike/002_file_reading run

test-spikes: spike-001 spike-002 ## Run every spike
