# sqlite-rs

.DEFAULT_GOAL := help

.PHONY: help test test-lib test-doc test-proptest test-isolation loc lint deny grammar-drift mvl-limit version version-pin mod-files verification verify fixtures fixtures-bench bench bench-cli bench-status sql-corpus test-corpus test-parity sqllogictest test-tiers test-point-lookup-perf test-spikes assurance assurance-gate traceability coverage coverage-gate mutants fuzz-btree fuzz-wal fuzz-decode-record fuzz-parse-select spike-001 spike-002 spike-003 spike-004 spike-005 spike-006 spike-007 spike-008 spike-009 opcodes silent-swallow

# Qualified-subset gate (issue #23). Boundary policy:
#   - Tier 0 core (src/record/, src/btree/, src/header.rs, schema reader):
#     stays limit-clean, no exceptions.
#   - src/vfs/ is the designated `dyn` boundary (its `Vfs`/`VfsFile`/
#     `SharedLockGuard` trait objects): exclude exactly that module here so
#     the claim stays explicit — everything above the VFS is in the
#     qualified subset. It no longer needs `unsafe` (#66): `fcntl`/`-shm`
#     access goes through safe `nix`/`std` APIs, and the crate is
#     `#![forbid(unsafe_code)]` with no local override anywhere.
#   - src/vdbe/exec.rs and src/vdbe/cursor.rs carry that same VFS boundary
#     one level up, as `Rc<dyn PageSource>` (#90, permanent per ADR-0013,
#     #114 considered and rejected). The erasure is the point:
#     a `Vm` holds at most one `Option<VmDb>` page source and clones it
#     cheaply into N open cursors, so they never contend over exclusive
#     ownership of the file handle. Making `Vm` generic over `P: PageSource`
#     instead would force `Vm::new()` — the no-database constructor every
#     arithmetic/control/sorter program uses — to name a concrete `P`, drop
#     its `Default` derive, and thread `<P: PageSource>` through every opcode
#     handler. That trades a justified trait object for pervasive generic
#     noise, so the boundary moves rather than the design.
#   - tests/spike/** is exempt: spikes are throwaway by design.
#
# Everything NOT listed here is a hard claim. Tier 0 core in particular has
# no exclusions: `src/btree.rs`'s `prev()` precondition is enforced with a
# real `BtreeError::CursorNotPositioned` rather than a `debug_assert!`
# precisely so the file stays limit-clean (and so the check survives into
# release builds).
MVL_LIMIT ?= cargo-mvl-limit
MVL_LIMIT_EXCLUDE := src/vfs.rs src/vfs/memory.rs src/vfs/unix.rs src/vfs/page_source.rs src/vdbe/exec.rs src/vdbe/cursor.rs src/bin/*

COVERAGE_MIN := 75

help: ## Show this help
	@echo ""
	@awk 'BEGIN {FS = ":.*?## "} \
	  /^# === .* ===$$/  { sub(/^# === /, ""); sub(/ ===$$/, ""); printf "\n\033[33m%s\033[0m\n", $$0 } \
	  /^[a-zA-Z0-9_-]+:.*?## / { printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2 }' \
	  $(MAKEFILE_LIST)
	@echo ""

# === Test ===

test: ## Run every test except the corpus oracle diffs (unit, public-API, proptest, doctests — see test-corpus)
	@# Build lock_probe helper binary first — cargo test doesn't build [[bin]] targets automatically
	cargo build --locked --bin lock_probe
	cargo test --locked

# Shortcuts for tight inner loops. Deliberately NOT dependencies of `test`:
# it stays a plain `cargo test` so that adding a test file can never drop it
# from the suite. `--lib --bins --doc` together cover only three of cargo's
# target kinds and would miss every `tests/*.rs` integration target — which
# is exactly how `record_proptest` went unrun for as long as it did.
test-lib: ## Just the library unit tests (fastest inner loop)
	cargo test --locked --lib

test-doc: ## Just the doctests
	cargo test --locked --doc

test-proptest: ## Just the property tests
	cargo test --locked --test record_proptest

test-isolation: ## Just the Tier 0 layer isolation guard (spec 001-architecture Req 1, #182)
	cargo test --locked --test unit_layer_isolation

test-corpus: ## Run the fixture corpus / oracle harness against a pinned real sqlite3 (see .openspec/specs/004-corpus)
	cargo test --locked --test corpus

test-parity: ## Run the per-V-block parity mirror against a pinned real sqlite3 (see #72)
	cargo test --locked --test parity

sqllogictest: ## Run the sqllogictest slice against a pinned real sqlite3, refreshing tools/sqllogictest-status.json (#96)
	cargo test --locked --test sqllogictest

test-tiers: ## Run the tier conformance suite standalone (tier0..tier3 — see .openspec/specs/001-architecture Tier Model)
	cargo test --locked --test tier0 --test tier1 --test tier2 --test tier3

test-point-lookup-perf: ## Quick wall-clock demos: rowid seek vs scan (#137), and indexed vs unindexed JOIN lookup (V4)
	cargo test --locked --test point_lookup_perf -- --nocapture


verification: test ## Verification level of the assurance case (alias for `make test`)

# === Gates ===
#
# Everything here is a pass/fail check intended to block a merge. Keep them
# fast and hermetic: the PR gate is only useful if it is cheap enough that
# nobody is tempted to skip it.

loc: ## Print lines-of-code stats for src/ vs tests/, separately (requires tokei)
	@command -v tokei >/dev/null 2>&1 || { \
	  echo "error: tokei not found. install: brew install tokei (or cargo install tokei)"; \
	  exit 1; }
	@echo "--- src/ (implementation) ---"
	@tokei src
	@echo "--- tests/ (test code) ---"
	@tokei tests

lint: ## Run clippy and check formatting
	# Deliberately `--lib --bins --tests --examples`, not `--all-targets`:
	# benches (tests/performance/engine.rs, #111/#112) need rusqlite linked
	# against the pinned oracle via `tools/bench_env.sh`, not whatever
	# sqlite3-dev a CI runner happens to ship — performance testing is a
	# manual `make bench`/`make bench-cli` workflow, not part of the
	# regular CI gate, so it deliberately isn't wired up here.
	cargo clippy --locked --lib --bins --tests --examples -- -D warnings
	# `[[test]] test = false` targets (corpus/parity/sqllogictest/
	# point_lookup_perf) opt out of the default `cargo test` run (see
	# their Cargo.toml comments) but `--tests` above doesn't build or
	# lint them either — they went uncompiled and unlinted for a while
	# as a result (#299: a stale `FromClause` field reference in
	# sqllogictest/runner.rs was a genuine compile error invisible to
	# every gate above until `cargo clippy --test sqllogictest` was run
	# directly). Named explicitly rather than discovered, matching how
	# `--tests` itself isn't a wildcard either.
	cargo clippy --locked --test corpus --test parity --test sqllogictest --test point_lookup_perf -- -D warnings
	cargo fmt -- --check

deny: ## Supply-chain gate: advisories, licenses, bans, sources (deny.toml)
	@command -v cargo-deny >/dev/null 2>&1 || { \
	  echo "error: cargo-deny not found."; \
	  echo "install: cargo install cargo-deny"; \
	  exit 1; }
	cargo deny check

grammar-drift: ## Grammar gate: .openspec/grammar/sqlite.ebnf annotations must resolve against pinned parse.y
	@python3 tools/grammar_drift.py --strict

mvl-limit: ## Qualified-subset gate: no unsafe/dyn/lifetimes in src/ (mvl-rust rust-limit; the 4 files with genuine dyn Vfs/VfsFile/SharedLockGuard trait objects, the 2 VDBE files with the Rc<dyn PageSource> boundary (#90, #114), and src/bin (stdout/stderr CLI I/O boundary), exempt — #66 removed the unsafe rationale from src/vfs/lock.rs, shm.rs, test_lock_probe.rs, so those are back in the qualified subset)
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

silent-swallow: ## Robustness audit: count error-discarding patterns in src/ (#342); VERBOSE=1 for file:line listing
	@echo "let _ = ...        (should be ~0 — clippy::let_underscore_must_use denies this, #343)"
	@grep -rn "let _ = " src/ $(if $(VERBOSE),,| wc -l | sed 's/^/  /') || true
	@echo "---"
	@echo ".ok()               (Result -> Option, error silently discarded)"
	@grep -rn "\.ok()" src/ $(if $(VERBOSE),,| wc -l | sed 's/^/  /') || true
	@echo "---"
	@echo ".unwrap_or(...)     (fallible call papered over with a default)"
	@grep -rn "\.unwrap_or" src/ $(if $(VERBOSE),,| wc -l | sed 's/^/  /') || true

version: ## Print the crate's current version (Cargo.toml [package].version)
	@sed -n 's/^version *= *"\([^"]*\)".*/\1/p' Cargo.toml | head -1

version-pin: ## Version gate: every sqlite3 pin site agrees with Cargo.toml's [package.metadata.oracle]
	python3 tools/version_pin.py --strict

mod-files: ## Module-layout gate: no legacy foo/mod.rs files under src/ (#73; use foo.rs instead)
	@hits=$$(find src -name 'mod.rs'); \
	if [ -n "$$hits" ]; then \
	  echo "MOD-FILE VIOLATION: legacy mod.rs found (use foo.rs instead):"; \
	  echo "$$hits"; \
	  exit 1; \
	fi; \
	echo "mod-files: no legacy mod.rs under src/"

# === Fixtures ===

fixtures: ## Regenerate the fixture corpus (tests/corpus/fixtures/) from tools/gen_fixtures.sh
	./tools/gen_fixtures.sh

opcodes: ## Harvest V2 (single-table SELECT) opcodes via pinned oracle EXPLAIN, write tools/opcodes-v2.json (spike 007, #58; needs a pinned, non-codec sqlite3 matching Cargo.toml's [package.metadata.oracle] version — override with --oracle)
	python3 tools/harvest_opcodes.py

sql-corpus: ## Regenerate tests/corpus/sql/{select,insert,update,delete,ddl}/ from the vendored sqllogictest + TCL subsets (#70; offline. Add FETCH=1 to refresh the vendored subsets from upstream)
	python3 tools/extract_sql_corpus.py $(if $(FETCH),--fetch,)

# === Bench (#111/#112 — three-tier perf regime) ===

fixtures-bench: ## Regenerate the ~1MB/~50MB bench fixtures (target/bench-fixtures/, not committed) from tools/gen_fixtures.sh --bench
	./tools/gen_fixtures.sh --bench

bench: fixtures-bench ## Tier 1 (engine-to-engine): criterion bench, sqlite-rs vs rusqlite linked to the pinned oracle (tests/performance/engine.rs)
	@bash -c '. ./tools/bench_env.sh && cargo bench --bench engine'

bench-cli: fixtures-bench ## Tier 2 (CLI-to-CLI): hyperfine, sqlite-rs dump/query vs sqlite3 (tools/bench_cli.sh)
	./tools/bench_cli.sh

bench-status: ## Assemble tools/bench-status.json from the latest `make bench`/`make bench-cli` raw output
	python3 tools/bench_status.py

# === Assurance ===

assurance: ## Assurance dashboard: spec -> code -> test traceability + evidence, with per-requirement/model detail
	@python3 tools/assurance.py --verbose

assurance-gate: ## CI gate: fail if completeness or scenario-weighted coverage is below 75%
	@python3 tools/assurance.py --min 0.75

traceability: ## Fast path: traceability only, no corpus/coverage I/O
	@python3 tools/assurance.py --traceability-only $(if $(VERBOSE),--verbose)

coverage: ## Run the test suite under coverage instrumentation and print a line coverage report (cargo-llvm-cov)
	# Two instrumented runs merged into one report. The corpus harness is
	# `test = false` in Cargo.toml, so `cargo test` skips it by default and
	# it must be named explicitly — otherwise every line only the oracle
	# diffs reach would silently read as uncovered. `clean` first, then
	# accumulate with `--no-report`, per cargo-llvm-cov's documented
	# merge-multiple-runs workflow.
	cargo llvm-cov clean --workspace
	cargo llvm-cov --locked --no-report
	cargo llvm-cov --locked --no-report --test corpus
	cargo llvm-cov report
	cargo llvm-cov report --json --output-path target/llvm-cov.json

coverage-gate: coverage ## CI gate: fail if line coverage is below $(COVERAGE_MIN)%
	@python3 -c "import json, sys; \
	  p = json.load(open('target/llvm-cov.json'))['data'][0]['totals']['lines']['percent']; \
	  print(f'Line coverage: {p:.2f}% (threshold: $(COVERAGE_MIN)%)'); \
	  sys.exit(0 if p >= $(COVERAGE_MIN) else 1)"

verify: coverage-gate deny mvl-limit mod-files ## Full verification gate (coverage-gate + deny + mvl-limit + mod-files), cached to target/verify.json
	@mkdir -p target
	@echo "{\"commit\": \"$$(git rev-parse HEAD)\", \"timestamp\": \"$$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" > target/verify.json
	@echo "make verify: all gates passed, recorded at $$(git rev-parse --short HEAD)"

# Scoped, not full-crate: a whole-crate mutation run is a documented V1
# exit-gate deliverable (epic #5, .openspec/plan.md) still out of scope —
# see the v0.12.0 changelog entry. MUTANTS_FILE defaults to the core
# logic modules a prior scoped run already covered (record decode,
# b-tree write path, VDBE write-opcode dispatch); override for a
# different slice, e.g. `make mutants MUTANTS_FILE='src/codegen/*.rs'`.
MUTANTS_FILE ?= src/{record,btree,vdbe}/*.rs
mutants: ## Run cargo-mutants over $(MUTANTS_FILE), report to target/mutants.out (scoped — see comment above)
	@command -v cargo-mutants >/dev/null 2>&1 || { \
	  echo "error: cargo-mutants not found."; \
	  echo "install: cargo install cargo-mutants"; \
	  exit 1; }
	cargo mutants --output target -f "$(MUTANTS_FILE)"

# === Fuzz ===

FUZZ_SECONDS ?= 60

fuzz-btree: ## Run the b-tree cursor fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration)
	cargo +nightly fuzz run --fuzz-dir tests/fuzz btree_cursor -- -max_total_time=$(FUZZ_SECONDS)

fuzz-wal: ## Run the WAL frame parsing fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration)
	cargo +nightly fuzz run --fuzz-dir tests/fuzz wal_frames -- -max_total_time=$(FUZZ_SECONDS)

fuzz-decode-record: ## Run the record-decoder fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration; discharges spec 003 Req 6)
	cargo +nightly fuzz run --fuzz-dir tests/fuzz decode_record -- -max_total_time=$(FUZZ_SECONDS)

fuzz-parse-select: ## Run the SELECT-core parser fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration; discharges spec 002 Req 2-4)
	cargo +nightly fuzz run --fuzz-dir tests/fuzz parse_select -- -max_total_time=$(FUZZ_SECONDS)


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

spike-006: ## Run spike 006 — grammar-slice viability for SELECT core (tests/spike/006_grammar_slice, issue #57)
	cd tests/spike/006_grammar_slice && cargo test

spike-007: opcodes ## Run spike 007 — opcode harvest via oracle EXPLAIN (tests/spike/007_opcode_harvest, #58; alias for `make opcodes`)

spike-008: ## Run spike 008 — tree-walking evaluator kernel-consumer prototype (tests/spike/008_tree_walker, #59)
	cd tests/spike/008_tree_walker && cargo test

spike-009: ## Run spike 009 — VFS dyn-elimination option A/B prototypes (tests/spike/009_vfs_dyn_elimination, #80)
	cd tests/spike/009_vfs_dyn_elimination && cargo run --bin option_a && cargo run --bin option_b

test-spikes: spike-001 spike-002 spike-003 spike-004 spike-005 spike-006 spike-007 spike-008 spike-009 ## Run every spike
