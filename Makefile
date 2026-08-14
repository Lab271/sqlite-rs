# sqlite-rs

.DEFAULT_GOAL := help

.PHONY: help test lint deny mvl-limit verification fixtures test-corpus test-spikes assurance assurance-gate traceability coverage coverage-gate fuzz-btree fuzz-wal fuzz-decode-record spike-001 spike-002 spike-003 spike-004 spike-005

# Qualified-subset gate (issue #23). Boundary policy:
#   - Tier 0 core (src/record/, src/btree/, src/header.rs, schema reader):
#     stays limit-clean, no exceptions.
#   - src/vfs/ (and later pager locking) is the designated `unsafe` boundary;
#     when it lands, exclude exactly that module here so the claim stays
#     explicit: everything above the VFS is in the qualified subset.
#   - tests/spike/** is exempt: spikes are throwaway by design.
MVL_LIMIT ?= cargo-mvl-limit
MVL_LIMIT_EXCLUDE := src/vfs/*

COVERAGE_MIN := 75

help: ## Show this help
	@echo ""
	@awk 'BEGIN {FS = ":.*?## "} \
	  /^# === .* ===$$/  { sub(/^# === /, ""); sub(/ ===$$/, ""); printf "\n\033[33m%s\033[0m\n", $$0 } \
	  /^[a-zA-Z0-9_-]+:.*?## / { printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2 }' \
	  $(MAKEFILE_LIST)
	@echo ""

# === Test ===

test: ## Run the unit test suite + public-API tests (excludes tests/corpus — see test-corpus)
	cargo test --locked --lib --bins
	cargo test --locked --test unit_header --test unit_record --test unit_vfs

lint: ## Run clippy and check formatting
	cargo clippy --locked --all-targets -- -D warnings
	cargo fmt -- --check

deny: ## Supply-chain gate: advisories, licenses, bans, sources (deny.toml)
	@command -v cargo-deny >/dev/null 2>&1 || { \
	  echo "error: cargo-deny not found."; \
	  echo "install: cargo install cargo-deny"; \
	  exit 1; }
	cargo deny check

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

fixtures: ## Regenerate the fixture corpus (tests/corpus/fixtures/) from tools/gen_fixtures.sh
	./tools/gen_fixtures.sh

test-corpus: ## Run the fixture corpus / oracle harness (see .openspec/specs/004-corpus)
	cargo test --locked --test corpus

# === Assurance ===

assurance: ## Assurance dashboard: spec -> code -> test traceability + evidence (VERBOSE=true for per-requirement detail)
	@python3 tools/assurance.py $(if $(VERBOSE),--verbose)

assurance-gate: ## CI gate: fail if completeness or scenario-weighted coverage is below 75%
	@python3 tools/assurance.py --min 0.75

traceability: ## Fast path: traceability only, no corpus/coverage I/O
	@python3 tools/assurance.py --traceability-only $(if $(VERBOSE),--verbose)

coverage: ## Run the test suite under coverage instrumentation and print a line coverage report (cargo-llvm-cov)
	cargo llvm-cov --locked --no-report
	cargo llvm-cov report
	cargo llvm-cov report --json --output-path target/llvm-cov.json

coverage-gate: coverage ## CI gate: fail if line coverage is below $(COVERAGE_MIN)%
	@python3 -c "import json, sys; \
	  p = json.load(open('target/llvm-cov.json'))['data'][0]['totals']['lines']['percent']; \
	  print(f'Line coverage: {p:.2f}% (threshold: $(COVERAGE_MIN)%)'); \
	  sys.exit(0 if p >= $(COVERAGE_MIN) else 1)"

# === Fuzz ===

FUZZ_SECONDS ?= 60

fuzz-btree: ## Run the b-tree cursor fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration)
	cd fuzz && cargo +nightly fuzz run btree_cursor -- -max_total_time=$(FUZZ_SECONDS)

fuzz-wal: ## Run the WAL frame parsing fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration)
	cd fuzz && cargo +nightly fuzz run wal_frames -- -max_total_time=$(FUZZ_SECONDS)

# === Spikes ===

spike-001: ## Run spike 001 — parser toolchain comparison (tests/spike/001_parser)
	$(MAKE) -C tests/spike/001_parser test

spike-002: ## Run spike 002 — file reading (tests/spike/002_file_reading)
	$(MAKE) -C tests/spike/002_file_reading run

spike-003: ## Run spike 003 — CSV export (tests/spike/003_csv_export)
	$(MAKE) -C tests/spike/003_csv_export run

spike-004: ## Run spike 004 — WAL frame reading (tests/spike/004_wal_reading)
	$(MAKE) -C tests/spike/004_wal_reading run

spike-005: ## Run spike 005 — locking protocol interop (tests/spike/005_locking_interop, issue #8)
	$(MAKE) -C tests/spike/005_locking_interop run

test-spikes: spike-001 spike-002 spike-003 spike-004 spike-005 ## Run every spike
