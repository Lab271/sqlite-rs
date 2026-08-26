---
domain: assurance
version: 0.1.0
status: draft
date: 2026-08-14
---

# 005 — Assurance

The living inventory of what defect class is caught at which phase, what
exists today, and what is deliberately deferred and until when. Backs V1
phase-1 (#26). Every future ticket that adds an assurance mechanism, or adds
code that an existing mechanism covers, MUST update this spec — this is the
living inventory; if it drifts from the Makefile/CI, the spec is wrong, not
the Makefile.

## Philosophy: Four Pillars

Shift-left principle (from MVL): catch each defect class at the earliest
phase that can catch it. The four phases are first-class assurance
arguments, not just a schedule:

| Pillar | Phase | Argument it makes | Our mechanisms |
|--------|-------|--------------------|-----------------|
| **1. Constructive** | Compile time | *Whole defect classes cannot exist* — correctness by construction | rustc ownership, `#![forbid(unsafe_code)]`, panic-surface lints, mvl-limit qualified subset (#23), (experiment) `#[mvl::total]` contracts |
| **2. Empirical** | Test time | *Behavior matches the oracle on evidence* | cargo test, llvm-cov, traceability dashboard (`tools/assurance.py`), corpus + pinned oracle (spec 004), proptest, cargo-fuzz — see harness taxonomy below |
| **3. Provenance** | Build time | *What we ship is what we tested* | Cargo.lock, `--locked` CI, cargo-deny, SHA-pinned GitHub Actions, pinned oracle (spec 004 Req 1) |
| **4. Operational** | Run time | *Failure is contained and explicit* | totality claim (any input -> `Ok` or structured `Err`, never panic/garbage — enforced at compile+fuzz time, claimed at run time), structured error taxonomy, `debug_assert!` invariants |

Each pillar trades against the others rather than duplicating them: Pillar 1
gives us, for free, what SQLite needs Valgrind/sanitizers (a separate
harness) to get — safe Rust makes memory-error detection a compile-time
property instead of a test-time one. Naming the pillars separately is what
makes that trade visible.

### Pillar 2: Harness Taxonomy

SQLite's credibility rests on **multiple independent harnesses with
different failure-mode sensitivities**, not one big suite
([sqlite.org/testing.html](https://www.sqlite.org/testing.html)). Design
rule for this project: **each harness must be justified by a failure mode
no other harness catches** — harness diversity is itself the assurance
argument (the same logic as diverse-lens verification in MVL).

| SQLite harness | What it's sensitive to | sqlite-rs counterpart |
|----------------|--------------------------|-------------------------|
| TCL suite (1,462 scripts, ~1M tests) | Feature regressions | Inline `#[cfg(test)]` + `tests/unit/` public-API tests (#30) |
| TH3 (proprietary, 100% MC/DC, 2.4M instances) | Untested branches, embedded configs | llvm-cov gate now; mutation testing (cargo-mutants) at V1 exit (epic #5) as our MC/DC-spirit metric |
| sqllogictest (7.2M queries) | Result divergence from other engines | Corpus + pinned-oracle diff harness (spec 004) — our center of gravity; full SLT run unlocks at V4 |
| dbsqlfuzz / AFL / OSS-Fuzz | Malformed-input crashes | cargo-fuzz on `decode_record` (#26), later on b-tree/WAL parsers |
| Anomaly testing (OOM injection, I/O fault, crash tests) | Failure-path bugs | V5 power-cut torture harness; fault-injecting VFS impl (the `Vfs` trait is our injection point) |
| Boundary/property testing | Edge values | proptest roundtrips (#26) |
| Valgrind/sanitizers | Memory errors | Largely subsumed by Pillar 1 (safe Rust) — crate-wide `#![forbid(unsafe_code)]` (#66) means there is no `unsafe` for miri to ever need to check |
| Disabled-optimization diff | Optimizer bugs | Future: planner-on vs planner-off result diffing (V4) — full scans as the reference implementation |

## Requirements

### Requirement 1: Four-Phase Inventory [MUST]

This spec MUST maintain a table naming, for every phase, what assurance
mechanisms are in place, what phase-1 of the current V-block adds, and what
is deliberately deferred with a named landing point. Deferred is never
dropped silently: moving a deferral's landing point is a plan change, not
an omission. Every ticket that adds an assurance mechanism, or adds code an
existing mechanism covers, MUST update this table in the same PR.

This file's inventory table is mirrored (summarized) in `.openspec/plan.md`'s Assurance Stack section.

**Implementation:** `.openspec/specs/005-assurance/spec.md` (planned)

This requirement is a process/review gate, not a program behavior: the
claim is that a markdown table in this file stays hand-updated whenever a
ticket adds an assurance mechanism. There is nothing in `src/` to test —
`tools/assurance.py` doesn't read or verify this table, it only walks
`### Requirement`/`#### Scenario` blocks — so it is excluded from the
Coverage score rather than carrying a link that would falsely claim
automated verification. Enforcement is PR review, per the maintenance
rule stated above.

#### Scenario: Inventory stays current

- GIVEN a ticket that adds a new assurance mechanism (e.g. a new lint, a new fuzz target, a new CI gate)
- WHEN the ticket closes
- THEN this spec's four-phase table reflects the addition — moved from "deferred" or added under the relevant phase

#### Scenario: Deferred items are traceable, not dropped

- GIVEN an item marked deferred (e.g. mutation testing, SBOM, `integrity_check`)
- WHEN a reader asks "why isn't this covered yet"
- THEN the table names the landing point (a V-block or epic), not just "later"

### Requirement 2: No-Panic Totality Claim [MUST]

The system MUST make the Tier 0 runtime claim — any input, however corrupt,
yields `Ok` or a structured `Err`, never a panic, never silent garbage — a
claim enforced at compile time (panic-surface lints) and test time
(exhaustive/fuzz coverage of decode paths), not merely asserted in
documentation.

Structured errors discharge this claim today; the panic-surface compile-time
gate and the decoder fuzz target land in #26 (existing non-test violations
in `src/header.rs`, `src/record/decode.rs`, `src/btree.rs`, and
`src/schema/ddl_reader.rs` must be refactored to a non-panicking form
first; `tests/fuzz/fuzz_targets/btree_cursor.rs` is the existing pattern the new
target follows).

**Implementation:** `src/record/error.rs`

**Tests:** `src/record/decode.rs::truncated_record_at_every_offset_errors_not_panics`

#### Scenario: Structured errors today

- GIVEN a malformed record payload (truncated body, header longer than payload)
- WHEN `decode_record` runs
- THEN it returns a `RecordError` variant naming the failure, never panics

#### Scenario: Compile-time panic-surface gate

- GIVEN `src/`'s workspace lint configuration
- WHEN `clippy::unwrap_used`, `clippy::expect_used`, `clippy::indexing_slicing`, `clippy::panic`, and `clippy::arithmetic_side_effects` are set to `deny`
- THEN `make lint` fails on any new panic-surface code in `src/` before it ships

**Tests:** `Cargo.toml` (planned)

#### Scenario: Fuzz target on the decoder

- GIVEN arbitrary bytes fed to `decode_record`
- WHEN run under `cargo fuzz run decode_record`
- THEN no panic is ever found — this discharges spec 003 Requirement 6's "Fuzz safety" scenario directly

**Tests:** `tests/fuzz/fuzz_targets/decode_record.rs` (planned)

### Requirement 3: Supply-Chain Gates [MUST]

The build MUST be provenance-checked: locked dependency resolution,
license/advisory scanning, and CI actions pinned to a commit SHA rather
than a mutable tag — all installed at the cheapest possible moment (zero
runtime dependencies today).

Locked resolution (`Cargo.lock`) exists today; `--locked` CI enforcement,
`deny.toml`, and SHA-pinned actions (per the Lab271 SOP, noting the
documented container-action exception) land in #26.

**Implementation:** `Cargo.lock`, `deny.toml`, `.github/workflows/ci.yml`

#### Scenario: Locked, advisory-clean build

- GIVEN a PR that adds or bumps a dependency
- WHEN CI runs
- THEN the build uses `--locked` (fails if `Cargo.lock` is stale) and `cargo deny check` passes (no known advisories, license violations, or banned crates)

**Tests:** `tests/unit/supply_chain_gates.rs::ci_enforces_locked_resolution_and_deny_check`

#### Scenario: Actions pinned by SHA

- GIVEN any GitHub Actions workflow step that isn't a container action
- WHEN reviewed
- THEN its `uses:` line references a commit SHA, not a mutable tag (`@v4`), per the Lab271 SOP

**Tests:** `tests/unit/supply_chain_gates.rs::every_non_container_action_is_pinned_to_a_commit_sha`

### Requirement 4: Contract Experiment [MAY]

The project MAY run `cargo mvl total` against `src/record/`'s decoder entry
points as a timeboxed experiment (half a day), annotating them
`#[mvl::total]` and running mvl-rust's panic-scan. Either outcome is
valuable: a clean pass is real-code validation for mvl-rust; a finding is
filed upstream (in the style of mvl#2284) and noted here.

**Finding (#26, mvl-rust v1.8.1 as installed): the experiment as scoped
doesn't apply to this codebase yet.** `mvl` (v1.8.1) is a compiler/toolchain
for the standalone MVL language — `mvl build`/`mvl check`/`mvl test` all
operate on `.mvl` source files and transpile them to Rust. There is no
`cargo mvl total` subcommand, no `#[mvl::total]` attribute, and no published
`mvl` crate on crates.io a plain Rust crate could depend on to annotate
existing `.rs` files in place. This is a different tool from
`cargo-mvl-limit` (the qualified-subset gate, #23) — that one *does* scan
real `.rs` files directly, because it's a narrower, standalone static
checker, not the full MVL transpiler. Confirmed by: `mvl --help`'s full
subcommand list (no `total`), `cargo add mvl --dry-run` (no such crate on
crates.io), and the existing `cargo-mvl-limit` binary's own scope (whole-file
scan, not annotation-based). This is the "doesn't work" branch the ticket
anticipated — the real-code-validation value returns once mvl-rust ships a
way to apply `#[mvl::total]`-style contracts to existing Rust source (or a
`src/record/` port to `.mvl` itself becomes in scope), whichever comes
first; re-attempt then rather than on a fixed date.

**Implementation:** `src/record/decode.rs` (not applicable — see finding above)

This requirement has no scenarios to test — the finding above, concluding
the experiment doesn't apply to the installed mvl-rust v1.8.1, is itself
the deliverable.

**Tests:** `.openspec/specs/005-assurance/spec.md`
