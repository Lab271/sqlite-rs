# sqlite-rs

.DEFAULT_GOAL := help

.PHONY: bench-compile-path help test test-lib test-doc test-proptest test-isolation loc lint hooks-install check-deny check-audit check-license-headers update vendor sbom sbom-dev supply-chain check-grammar-drift check-mvl-limit version version-pin check-mod-files verification verify fixtures fixtures-bench bench bench-cli bench-status bench-point-lookup extract-sql-corpus test-corpus test-parity test-sqllogictest test-tcl test-tiers test-spikes test-mcdc mcdc-obligations assurance check-assurance traceability coverage check-coverage mutants fuzz-btree fuzz-wal fuzz-decode-record fuzz-parse-select fuzz-scalar-functions fuzz-vdbe-exec fuzz-semantics-compare fuzz-smoke spike-001 spike-002 spike-003 spike-004 spike-005 spike-006 spike-007 spike-008 spike-009 opcodes silent-swallow docs docs-serve

# Qualified-subset gate (issue #23). Boundary policy:
#   - Tier 0 core (src/record/, src/btree/, src/header.rs, schema reader):
#     stays limit-clean, no exceptions.
#   - src/vfs/ is the designated `dyn` boundary (its `Vfs`/`VfsFile`/
#     `SharedLockGuard` trait objects): exclude exactly that module here so
#     the claim stays explicit — everything above the VFS is in the
#     qualified subset. It no longer needs `unsafe` itself (#66): `fcntl`/
#     `-shm` access goes through the safe wrappers in `src/sys/` (#563,
#     vendored FFI — the crate's sole `#![allow(unsafe_code)]` carve-out;
#     see .openspec/adr/0031-vendor-nix-subset.md). `src/lib.rs` is
#     `#![deny(unsafe_code)]` everywhere else, with no override possible
#     outside `src/sys/`.
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
MVL_LIMIT_EXCLUDE := src/vfs.rs src/vfs/memory.rs src/vfs/unix.rs src/vfs/page_source.rs src/vdbe/exec.rs src/vdbe/cursor.rs src/bin/* src/sys.rs src/sys/*

COVERAGE_MIN := 80

help: ## Show this help
	@echo ""
	@awk 'BEGIN {FS = ":.*?## "} \
	  /^# === .* ===$$/  { sub(/^# === /, ""); sub(/ ===$$/, ""); printf "\n\033[33m%s\033[0m\n", $$0 } \
	  /^[a-zA-Z0-9_-]+:.*?## / { printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2 }' \
	  $(MAKEFILE_LIST)
	@echo ""

# === Docs ===

docs: ## Build the mdBook documentation site (docs/book)
	mdbook build docs

docs-serve: ## Serve the mdBook documentation site locally with live reload
	mdbook serve docs --open

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

test-sqllogictest: ## Run the sqllogictest slice against a pinned real sqlite3, refreshing tools/sqllogictest-status.json (#96)
	cargo test --locked --test sqllogictest -- --nocapture

test-tcl: ## Run the TCL-sourced extracted-SQL corpus checks (tokenizer totality, no-false-invalid) standalone
	cargo test --locked --test extracted_sql_corpus tcl -- --nocapture

test-tiers: ## Run the tier conformance suite standalone (tier0..tier3 — see .openspec/specs/001-architecture Tier Model)
	cargo test --locked --test tier0 --test tier1 --test tier2 --test tier3

# Scanned file set for `test-mcdc`. Grown module-by-module as tagged
# obligations land: btree (#52), then vdbe/functions, parser/grammar,
# parser/tokenizer, vdbe/exec, record/encode (#368), then vdbe/program +
# vdbe/control (opcode dispatch, fix/mcdc-scope).
MCDC_FILES := src/btree.rs src/btree/*.rs src/btree/table/*.rs src/btree/index/*.rs \
	src/vdbe/functions.rs src/parser/grammar.rs src/parser/tokenizer.rs \
	src/vdbe/exec.rs src/record/encode.rs \
	src/vdbe/program.rs src/vdbe/control.rs

# Committed obligations snapshot (tests/mcdc/obligations.json), analogous
# to the corpus fixtures (spec 004): checked into git so
# `unit_mcdc_discharge`'s tagged-test-vs-real-obligation check runs at
# normal `cargo test` time with no external tool — cargo-mvl-mcdc is only
# needed to *regenerate* the snapshot, same "committed evidence,
# regenerable on demand" split as `make fixtures`/`tools/gen_fixtures.sh`.
# Regenerate whenever a code change shifts a decision's line number
# (obligation ids are `<file>_<line>`) — `unit_mcdc_discharge` fails
# loudly, naming the stale id, if a tagged test's id no longer resolves.
mcdc-obligations: ## Regenerate the committed MC/DC obligations snapshot (tests/mcdc/obligations.json)
	@command -v cargo-mvl-mcdc >/dev/null 2>&1 || { \
		echo "cargo-mvl-mcdc not found — install with:"; \
		echo "  cargo install --git https://github.com/mvl-lang/mvl-rust rust-mcdc --bin cargo-mvl-mcdc"; \
		exit 1; \
	}
	@mkdir -p tests/mcdc
	cargo-mvl-mcdc scan -o tests/mcdc/obligations.json $(MCDC_FILES)
	@echo "wrote tests/mcdc/obligations.json — commit it alongside the source change that shifted line numbers"

test-mcdc: mcdc-obligations ## MC/DC dashboard for the scanned file set; fails if any multi-leaf obligation is undischarged (VERBOSE=1 for per-obligation detail — #52, #368)
	# `harvest` re-runs `cargo test` itself and joins on tagged test names
	# regardless of overall suite pass/fail (per-test outcome, not exit
	# status) — the tagged tests are ordinary #[test] fns already run
	# under `make test`/`make test-lib`; this target is an additional
	# coverage *view*, not a separate test run.
	cargo-mvl-mcdc harvest --obligations=tests/mcdc/obligations.json --run-dir=. 2>/dev/null \
		| python3 tools/mcdc_report.py $(if $(filter 1,$(VERBOSE)),--verbose,)

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

hooks-install: ## Install git hooks (tools/hooks/) into the shared hooks dir — covers every worktree at once
	@HOOKS_DIR=$$(git rev-parse --git-common-dir)/hooks; \
	mkdir -p "$$HOOKS_DIR"; \
	for h in tools/hooks/*; do \
	  name=$$(basename "$$h"); \
	  ln -sf "$$(cd tools/hooks && pwd)/$$name" "$$HOOKS_DIR/$$name"; \
	  chmod +x "$$h"; \
	  echo "installed: $$HOOKS_DIR/$$name -> $$h"; \
	done

check-deny: ## Supply-chain gate: advisories, licenses, bans, sources (deny.toml)
	@command -v cargo-deny >/dev/null 2>&1 || { \
	  echo "error: cargo-deny not found."; \
	  echo "install: cargo install cargo-deny"; \
	  exit 1; }
	cargo deny check

check-audit: ## Supply-chain gate: fail on known RUSTSEC vulnerabilities (cargo-audit)
	@command -v cargo-audit >/dev/null 2>&1 || { \
	  echo "error: cargo-audit not found."; \
	  echo "install: cargo install cargo-audit"; \
	  exit 1; }
	cargo audit

check-license-headers: ## Supply-chain gate: every tracked .rs file (except vendored third_party) carries the Copyright/SPDX header
	python3 tools/license_headers.py

update: ## Supply-chain: cargo update, then re-run deny+audit against the new lockfile before you commit it
	@cp Cargo.lock Cargo.lock.before-update
	cargo update
	@echo ""; \
	if diff -q Cargo.lock.before-update Cargo.lock >/dev/null 2>&1; then \
	  echo "Cargo.lock unchanged."; \
	else \
	  echo "Cargo.lock changes:"; \
	  diff -u Cargo.lock.before-update Cargo.lock | grep -E '^[+-]name = |^[+-]version = ' | paste -d' ' - - || true; \
	fi
	@rm -f Cargo.lock.before-update
	@echo ""; echo "Re-checking updated lockfile:"
	@$(MAKE) check-deny
	@$(MAKE) check-audit
	@echo ""; echo "Lockfile updated and re-vetted — review the diff above, then commit Cargo.lock if it looks right."

vendor: ## Supply-chain: cargo vendor vendor/ for local inspection of exact upstream source (gitignored, not built from by default)
	cargo vendor vendor

# `--describe crate` (the default) walks the production dependency graph
# only — dev-dependencies never ship, so they're out of scope for an SBOM
# describing the trust boundary a downstream consumer inherits (same split
# DEPENDENCIES.md draws). SOURCE_DATE_EPOCH pinned to HEAD's commit time
# makes the output byte-reproducible (no timestamp/serialNumber churn) so
# `git diff` only shows real dependency changes, not regeneration noise.
sbom: ## Regenerate the CycloneDX SBOM (sqlite-rs.cdx.json) from the production dependency graph
	@command -v cargo-cyclonedx >/dev/null 2>&1 || { \
	  echo "error: cargo-cyclonedx not found."; \
	  echo "install: cargo install cargo-cyclonedx"; \
	  exit 1; }
	SOURCE_DATE_EPOCH=$$(git log -1 --format=%ct) cargo cyclonedx --format json --describe crate --spec-version 1.5
	@# cargo-cyclonedx bakes the absolute checkout path into every
	@# `bom-ref` (its `purl`s already correctly use relative `file://.`);
	@# an absolute, machine-/checkout-specific path in a committed file
	@# is both a reproducibility and a minor privacy leak (local
	@# username), so normalize it to the same relative form the tool
	@# itself uses for purls.
	@python3 -c "\
	import pathlib; \
	p = pathlib.Path('sqlite-rs.cdx.json'); \
	root = str(pathlib.Path.cwd()); \
	text = p.read_text().replace('path+file://' + root, 'path+file://.'); \
	p.write_text(text)"
	@echo "wrote sqlite-rs.cdx.json — commit it alongside any production dependency change"

# `cargo-cyclonedx` structurally excludes dev-dependencies from every
# describe mode it has (verified: --describe all-cargo-targets still
# omits them) — there's no flag to include them, scope-tagged or
# otherwise. Build-time code execution (a malicious build.rs, a
# compromised test harness) is a real attack vector independent of what
# ships, so tools/gen_dev_sbom.py reads `cargo metadata` directly and
# emits the full Cargo.lock closure instead, with each component's
# `scope` computed from whether it's reachable from the root by a chain
# of only normal-kind edges (`required`) or only via a dev/build edge
# somewhere in the chain (`optional`) — currently all 116 non-root
# packages are `optional`, matching zero production dependencies.
sbom-dev: ## Regenerate the dev-inclusive CycloneDX SBOM (sqlite-rs-dev.cdx.json) covering the full Cargo.lock closure
	python3 tools/gen_dev_sbom.py

supply-chain: check-deny check-audit check-license-headers ## All supply-chain gates (check-deny + check-audit + check-license-headers), cached to target/supply-chain.json for `make assurance` staleness reporting
	@mkdir -p target
	@echo "{\"commit\": \"$$(git rev-parse HEAD)\", \"timestamp\": \"$$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" > target/supply-chain.json
	@echo "make supply-chain: deny + audit + license-headers passed, recorded at $$(git rev-parse --short HEAD)"

check-grammar-drift: ## Grammar gate: .openspec/grammar/sqlite.ebnf annotations must resolve against pinned parse.y
	@python3 tools/grammar_drift.py --strict

check-mvl-limit: ## Qualified-subset gate: no unsafe/dyn/lifetimes in src/ (mvl-rust rust-limit; the 4 files with genuine dyn Vfs/VfsFile/SharedLockGuard trait objects, the 2 VDBE files with the Rc<dyn PageSource> boundary (#90, #114), src/bin (stdout/stderr CLI I/O boundary), and src/sys/ (vendored fcntl/termios FFI, #563 — the crate's sole unsafe carve-out, see .openspec/adr/0031-vendor-nix-subset.md), exempt — #66 removed the unsafe rationale from src/vfs/lock.rs, shm.rs, test_lock_probe.rs, so those are back in the qualified subset)
	@command -v $(MVL_LIMIT) >/dev/null 2>&1 || { \
	  echo "error: $(MVL_LIMIT) not found."; \
	  echo "install: cargo install cargo-mvl  (or build from mvl-lang/mvl-rust:"; \
	  echo "         cargo build -p rust-limit --bin cargo-mvl-limit)"; \
	  exit 1; }
	@fail=0; \
	for f in $$(find src -name '*.rs' $(foreach e,$(MVL_LIMIT_EXCLUDE),-not -path '$(e)') | sort); do \
	  if ! $(MVL_LIMIT) "$$f"; then echo "LIMIT VIOLATION: $$f"; fail=1; fi; \
	done; \
	if [ $$fail -eq 0 ]; then echo "check-mvl-limit: all files in the qualified subset"; fi; \
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

check-mod-files: ## Module-layout gate: no legacy foo/mod.rs files under src/ (#73; use foo.rs instead)
	@hits=$$(find src -name 'mod.rs'); \
	if [ -n "$$hits" ]; then \
	  echo "MOD-FILE VIOLATION: legacy mod.rs found (use foo.rs instead):"; \
	  echo "$$hits"; \
	  exit 1; \
	fi; \
	echo "check-mod-files: no legacy mod.rs under src/"

# === Fixtures ===

fixtures: ## Regenerate the fixture corpus (tests/corpus/fixtures/) from tools/gen_fixtures.sh
	./tools/gen_fixtures.sh

opcodes: ## Harvest V2 (single-table SELECT) opcodes via pinned oracle EXPLAIN, write tools/opcodes-v2.json (spike 007, #58; needs a pinned, non-codec sqlite3 matching Cargo.toml's [package.metadata.oracle] version — override with --oracle)
	python3 tools/harvest_opcodes.py

extract-sql-corpus: ## Regenerate tests/corpus/sql/{select,insert,update,delete,ddl}/ from the vendored sqllogictest + TCL subsets (#70; offline. Add FETCH=1 to refresh the vendored subsets from upstream)
	python3 tools/extract_sql_corpus.py $(if $(FETCH),--fetch,)

# === Bench (#111/#112 — three-tier perf regime) ===

fixtures-bench: ## Regenerate the ~1MB/~50MB bench fixtures (target/bench-fixtures/, not committed) from tools/gen_fixtures.sh --bench
	./tools/gen_fixtures.sh --bench

bench: fixtures-bench ## Tier 1 (engine-to-engine): criterion bench, sqlite-rs vs rusqlite linked to the pinned oracle (tests/performance/engine.rs)
	@bash -c '. ./tools/bench_env.sh && cargo bench --bench engine'

bench-v6: fixtures-bench ## V6 (epic #354, #391): WAL journal-vs-WAL/concurrent-read-write/checkpoint + CTE-reuse benches (tests/performance/v6.rs)
	@bash -c '. ./tools/bench_env.sh && cargo bench --bench v6'

bench-skip-scan: ## #485: skip-scan vs full-scan at a low-cardinality leading index column (tests/performance/skip_scan.rs, own fixture)
	@bash -c '. ./tools/bench_env.sh && cargo bench --bench skip_scan'

bench-compile-path: ## #590: Tier 2 compile path (tokenize/parse/expand/codegen), sqlite-rs vs itself across revisions (tests/performance/compile_path.rs, no fixture or oracle needed)
	cargo bench --bench compile_path

bench-cli: fixtures-bench ## Tier 2 (CLI-to-CLI): hyperfine, sqlite-rs dump/query vs sqlite3 (tools/bench_cli.sh)
	./tools/bench_cli.sh

bench-status: ## Assemble tools/bench-status.json from the latest `make bench`/`make bench-cli` raw output
	python3 tools/bench_status.py

bench-point-lookup: ## Quick wall-clock demos: rowid seek vs scan (#137), and indexed vs unindexed JOIN lookup (V4)
	cargo test --locked --test point_lookup_perf -- --nocapture

# === Assurance ===

assurance: ## Assurance dashboard: spec -> code -> test traceability + evidence, with per-requirement/model detail
	@python3 tools/assurance.py --verbose

check-assurance: ## CI gate: fail if completeness or scenario-weighted coverage is below 80%
	@python3 tools/assurance.py --min 0.80

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

check-coverage: coverage ## CI gate: fail if line coverage is below $(COVERAGE_MIN)%
	@python3 -c "import json, sys; \
	  p = json.load(open('target/llvm-cov.json'))['data'][0]['totals']['lines']['percent']; \
	  print(f'Line coverage: {p:.2f}% (threshold: $(COVERAGE_MIN)%)'); \
	  sys.exit(0 if p >= $(COVERAGE_MIN) else 1)"

verify: check-coverage check-deny check-mvl-limit check-mod-files ## Full verification gate (check-coverage + check-deny + check-mvl-limit + check-mod-files), cached to target/verify.json
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

# Every target passes TWO corpus dirs: tests/fuzz/corpus/<name> first
# (the libFuzzer-grown corpus, gitignored, where newly discovered inputs
# get saved — libFuzzer always writes into the FIRST corpus dir it's
# given) and tests/fuzz/seeds/<name> second, read-only (real crash/
# regression inputs and a few hand-picked structurally-valid seeds,
# committed to git — see tests/fuzz/seeds/README.md). Passing only the
# seeds dir would make libFuzzer treat IT as the writable corpus instead
# and fill it with generated inputs — see the README's note (#615).
fuzz-btree: ## Run the b-tree cursor fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration)
	@mkdir -p tests/fuzz/corpus/btree_cursor
	cargo +nightly fuzz run --fuzz-dir tests/fuzz btree_cursor tests/fuzz/corpus/btree_cursor tests/fuzz/seeds/btree_cursor -- -max_total_time=$(FUZZ_SECONDS)

fuzz-wal: ## Run the WAL frame parsing fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration)
	@mkdir -p tests/fuzz/corpus/wal_frames
	cargo +nightly fuzz run --fuzz-dir tests/fuzz wal_frames tests/fuzz/corpus/wal_frames tests/fuzz/seeds/wal_frames -- -max_total_time=$(FUZZ_SECONDS)

fuzz-decode-record: ## Run the record-decoder fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration; discharges spec 003 Req 6)
	@mkdir -p tests/fuzz/corpus/decode_record
	cargo +nightly fuzz run --fuzz-dir tests/fuzz decode_record tests/fuzz/corpus/decode_record tests/fuzz/seeds/decode_record -- -max_total_time=$(FUZZ_SECONDS)

fuzz-parse-select: ## Run the SELECT-core parser fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration; discharges spec 002 Req 2-4)
	@mkdir -p tests/fuzz/corpus/parse_select
	cargo +nightly fuzz run --fuzz-dir tests/fuzz parse_select tests/fuzz/corpus/parse_select tests/fuzz/seeds/parse_select -- -max_total_time=$(FUZZ_SECONDS)

# -rss_limit_mb raised from libFuzzer's 2048 default: zeroblob() legitimately
# allocates up to MAX_BLOB_LEN (~1GB, matching SQLite's SQLITE_MAX_LENGTH
# default — see src/vdbe/functions.rs), and ASan's allocator quarantines
# freed blocks rather than returning them to the OS, so repeated ~1GB
# zeroblob calls across fuzz iterations accumulate RSS well past 2048MB
# with no actual leak. Confirmed not a real bug (src/vdbe/functions.rs's
# `zeroblob_clamps_oversized_length` test already covers the clamp).
fuzz-scalar-functions: ## Run the scalar-function dispatch fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration)
	@mkdir -p tests/fuzz/corpus/scalar_functions
	cargo +nightly fuzz run --fuzz-dir tests/fuzz scalar_functions tests/fuzz/corpus/scalar_functions tests/fuzz/seeds/scalar_functions -- -max_total_time=$(FUZZ_SECONDS) -rss_limit_mb=4096

fuzz-vdbe-exec: ## Run the VDBE opcode-execution fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration; discharges spec 009's no-panic-totality obligation, #89)
	@mkdir -p tests/fuzz/corpus/vdbe_exec
	cargo +nightly fuzz run --fuzz-dir tests/fuzz vdbe_exec tests/fuzz/corpus/vdbe_exec tests/fuzz/seeds/vdbe_exec -- -max_total_time=$(FUZZ_SECONDS)

fuzz-semantics-compare: ## Run the value-comparison total-order property fuzz target (requires cargo-fuzz + nightly; FUZZ_SECONDS to change duration; discharges spec 008 Req 2)
	@mkdir -p tests/fuzz/corpus/semantics_compare
	cargo +nightly fuzz run --fuzz-dir tests/fuzz semantics_compare tests/fuzz/corpus/semantics_compare tests/fuzz/seeds/semantics_compare -- -max_total_time=$(FUZZ_SECONDS)

FUZZ_TARGETS := btree_cursor wal_frames decode_record parse_select scalar_functions vdbe_exec semantics_compare
FUZZ_SMOKE_SECONDS ?= 15

fuzz-smoke: ## Short crash-only run of every fuzz target (CI gate; FUZZ_SMOKE_SECONDS per target, default 15s)
	@for t in $(FUZZ_TARGETS); do \
		echo "--- fuzz-smoke: $$t ($(FUZZ_SMOKE_SECONDS)s) ---"; \
		mkdir -p "tests/fuzz/corpus/$$t"; \
		cargo +nightly fuzz run --fuzz-dir tests/fuzz "$$t" "tests/fuzz/corpus/$$t" "tests/fuzz/seeds/$$t" \
			-- -max_total_time=$(FUZZ_SMOKE_SECONDS) -rss_limit_mb=4096 || exit 1; \
	done

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

spike-014: ## Run spike 014 — embedding-API kernel: Send+Sync handle over a streaming VDBE (tests/spike/014_embedding_api, #682)
	$(MAKE) -C tests/spike/014_embedding_api test

spike-014-bench: ## Bench spike 014 — full-drain vs time-to-first-row across batch/worker/stream/chunked (#682)
	$(MAKE) -C tests/spike/014_embedding_api bench

test-spikes: spike-001 spike-002 spike-003 spike-004 spike-005 spike-006 spike-007 spike-008 spike-009 ## Run every spike
