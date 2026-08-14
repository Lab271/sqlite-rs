---
domain: testing
version: 0.1.0
status: draft
date: 2026-08-13
---

# 004 — Corpus & Oracle

The fixture corpus and oracle harness — the evidence layer every other spec's **Tests:** links ultimately rest on. Backs V1 step 8 (#10). Stock SQLite is the oracle; this spec pins it and makes fixture generation reproducible.

## Philosophy

We do not define correctness — SQLite does. Every claim sqlite-rs makes is backed by a diff against a **pinned** oracle build. Spike 002 (#6) proved why pinning matters: macOS's system `sqlite3` is a codec build (see-cccrypt, 12 reserved bytes/page) that silently produces different files than stock SQLite. Spike #22 isolated the cause: compiling the same version (3.51.0) without the codec flag produces `reserved_space=0`, confirming the codec flag — not the sqlite3 version — is responsible, so a non-codec build is a sufficient pin regardless of version (see `tests/spike/002_file_reading/findings.md`, finding 1). Oracle drift is treated like a dependency bump: deliberate, reviewed, recorded.

## Requirements

### Requirement 1: Pinned Oracle [MUST]

Fixture generation and oracle diffs MUST use a pinned, non-codec sqlite3 build whose exact version is recorded in the harness. The harness MUST fail loudly when the available oracle differs from the pin. Oracle version bumps are explicit, reviewed changes.

**Implementation:** `tools/gen_fixtures.sh` (enforcement — codec and version checks run at fixture-generation time), `tests/corpus/oracle.rs` (pinned-version constant shared with tests)

**Tests:** `tests/corpus/oracle_test.rs`

#### Scenario: Version mismatch fails loudly

- GIVEN a machine whose pinned oracle binary reports a different version than the recorded pin
- WHEN the corpus harness starts
- THEN it aborts with an error naming both versions — no silent fallback to the system sqlite3

#### Scenario: Codec build rejected

- GIVEN macOS's system sqlite3 (compiled with `CODEC=see-cccrypt`)
- WHEN offered as the oracle
- THEN the harness rejects it (detects codec via reserved-bytes behavior or compile options)

### Requirement 2: Reproducible Fixture Generation [MUST]

The corpus MUST be regenerable deterministically from a script using the pinned oracle. Committed fixtures and regenerated fixtures MUST be functionally identical (same schema, same rows — byte-identity not required where sqlite3 embeds nondeterminism).

**Implementation:** `tools/gen_fixtures.sh`

**Tests:** `tests/corpus/regen_test.rs`

#### Scenario: Regeneration round-trip

- GIVEN a clean checkout
- WHEN `make fixtures` runs twice
- THEN both runs produce corpora that the harness reports as equivalent (byte-identical, except `journalstates/`'s WAL-salt/journal-nonce-bearing files, which are compared by size — see #35/#36)

**Tests:** `tests/corpus/regen_test.rs::regeneration_is_reproducible`

### Requirement 3: Fixture Families [MUST]

The corpus MUST contain fixtures for every Tier 0 format dimension. One family per dimension (a subdirectory under `tests/corpus/fixtures/`), each with a `manifest.txt` describing what it exercises.

**Implementation:** `tests/corpus/fixtures/`

**Tests:** `tests/corpus/families_test.rs`

#### Scenario: Serial type family

- GIVEN the `serialtypes/` family
- THEN it contains every serial type including `i64::MIN`/`MAX`, `-0.0`, huge floats, empty and 64-byte blobs, and NULL

#### Scenario: Encoding family

- GIVEN the `encodings/` family
- THEN it contains UTF-8, UTF-16LE, and UTF-16BE databases with identical logical content

#### Scenario: Page geometry family

- GIVEN the `pagesizes/` family
- THEN it contains page sizes 512 and 65536 (4096 is covered implicitly by every other family), and reserved-bytes variants 0 and 12

#### Scenario: B-tree shape family

- GIVEN the `btrees/` family
- THEN it contains single-page tables, multi-page tables (interior nodes), index b-trees, WITHOUT ROWID tables, and rows forcing single- and multi-page overflow chains

#### Scenario: Journal-state family

- GIVEN the `journalstates/` family
- THEN it contains a hot-journal (crashed rollback-journal writer) fixture and four WAL-pending fixtures (primary uncheckpointed case, trailing spilled-but-uncommitted frames, a stale foreign-generation frame, and the big-endian checksum path) — each produced by scripting a partial/interrupted write (a blocked reader/writer held open on a fifo) rather than a single deterministic `sqlite3` invocation, per spike #7's findings (#21, #35, #36)

**Tests:** `tests/corpus/families_test.rs::journal_states_family`

#### Scenario: Feature-bearing family

- GIVEN the `features/` family
- THEN it contains auto-vacuum, FTS5, R-Tree, STRICT, and generated-column databases — all raw-row readable per Tier 0 (spec 001 Requirement 4)

#### Scenario: Invalid-input family

- GIVEN the `invalid/` family (added beyond this spec's original scope, for #11's "rejects non-SQLite files" / "malformed headers: Err, never panic" acceptance criteria)
- THEN it contains a zero-byte file, a database truncated mid-header, and a database with a corrupted magic string

### Requirement 4: Oracle Diff Harness [MUST]

The harness MUST, for each fixture, compare sqlite-rs output against pinned-oracle output and report per-fixture pass/fail. It MUST run as a `cargo test` integration target and via `make test-corpus` (named to match the existing `test`/`test-spikes` Makefile convention, kept separate from `make test` so the fast unit-test loop doesn't pay for corpus discovery on every run). Fixtures for not-yet-implemented capabilities MUST be reported as skipped, not failed — the harness runs green from day one and fills in as steps land.

**Implementation:** `tests/corpus/harness.rs`

**Tests:** `tests/corpus/harness_test.rs`

#### Scenario: Green with stub reader

- GIVEN the harness before any reader code exists
- WHEN `make test-corpus` runs
- THEN all fixtures report SKIPPED and the run exits 0

#### Scenario: Diff failure is actionable

- GIVEN a fixture where sqlite-rs output diverges from the oracle
- WHEN the harness reports it
- THEN the report names the fixture, the first diverging row/value, and both outputs
