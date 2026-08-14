# sqlite-rs

.DEFAULT_GOAL := help

.PHONY: help test lint mvl-limit verification test-spikes assurance assurance-gate traceability coverage coverage-gate spike-001 spike-002

# Qualified-subset gate (issue #23). Boundary policy:
#   - Tier 0 core (src/record/, src/btree/, src/header.rs, schema reader):
#     stays limit-clean, no exceptions.
#   - src/vfs/ (and later pager locking) is the designated `unsafe` boundary;
#     when it lands, exclude exactly that module here so the claim stays
#     explicit: everything above the VFS is in the qualified subset.
#   - tests/spike/** is exempt: spikes are throwaway by design.
MVL_LIMIT ?= cargo-mvl-limit
MVL_LIMIT_EXCLUDE :=

COVERAGE_MIN := 75

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

lint: ## Run clippy and check formatting
	cargo clippy --all-targets -- -D warnings
	cargo fmt -- --check

verification: test ## Verification level of the assurance case (alias for `make test`)

mvl-limit: ## Qualified-subset gate: no unsafe/dyn/lifetimes in src/ (mvl-rust rust-limit; spikes exempt)
	@command -v $(MVL_LIMIT) >/dev/null 2>&1 || { \
	  echo "error: $(MVL_LIMIT) not found."; \
	  echo "install: cargo install cargo-mvl  (or build from mvl-lang/mvl-rust:"; \
	  echo "         cargo build -p rust-limit --bin cargo-mvl-limit)"; \
	  exit 1; }
	@fail=0; \
	for f in $$(find src -name '*.rs' $(foreach e,$(MVL_LIMIT_EXCLUDE),-not -path '$(e)') | sort); do \
	  if ! $(MVL_LIMIT) "$$f"; then echo "LIMIT VIOLATION: $$f"; fail=1; fi; \
	done; \
	if [ $$fail -eq 0 ]; then echo "mvl-limit: all files in the qualified subset"; fi; \
	exit $$fail

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

coverage-gate: coverage ## CI gate: fail if line coverage is below $(COVERAGE_MIN)%
	@python3 -c "import json, sys; \
	  p = json.load(open('target/llvm-cov.json'))['data'][0]['totals']['lines']['percent']; \
	  print(f'Line coverage: {p:.2f}% (threshold: $(COVERAGE_MIN)%)'); \
	  sys.exit(0 if p >= $(COVERAGE_MIN) else 1)"

# === Spikes ===

spike-001: ## Run spike 001 — parser toolchain comparison (tests/spike/001_parser)
	$(MAKE) -C tests/spike/001_parser test

spike-002: ## Run spike 002 — file reading (tests/spike/002_file_reading)
	$(MAKE) -C tests/spike/002_file_reading run

test-spikes: spike-001 spike-002 ## Run every spike
