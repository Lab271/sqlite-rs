# sqlite-rs

A binary-compatible Rust replication of SQLite. See `AGENTS.md` for the
worktree/agent-branch workflow.

## Token spend policy

Every ticket (GitHub issue) must secure its token spend up front and account
for it on close:

- **Before starting work** (`/take`), the issue must carry an explicit
  complexity/effort estimate (the `## Complexity` section produced by
  `/issue`). Treat that estimate as the token budget for the ticket — if the
  work is ballooning past it, stop and re-scope with the user rather than
  quietly continuing.
- **Large or multi-agent work** (via the `Workflow` tool) must pass an
  explicit `budget.total` (from a user "+Nk tokens" directive, or a sane
  default sized to the ticket's complexity estimate) so spend is capped, not
  open-ended.
- **On close** (PR description or issue closing comment), note actual spend
  or effort relative to the estimate — one line is enough (e.g. "spend:
  matched estimate" or "spend: 2x estimate because X"). This keeps AI usage
  cost visible per ticket instead of only visible in aggregate.

This applies to every ticket, not just large ones — trivial tickets just get
a trivial budget.

## Spec traceability conventions

- **Tickets cite requirement IDs, machine-greppably.** Every feature ticket
  body carries a line like `Refs: 003/Req-4, 003/Req-6` naming the spec
  requirements it implements. `gh issue list --search "003/Req-4"` (or grep
  of exported issues) then answers "which ticket implemented Req 4" — no
  extra tooling.
- **Test links are per-scenario, not per-requirement.** When closing a
  ticket, add a `**Tests:**` line INSIDE each `#### Scenario:` block it
  discharges, pointing at the concrete test
  (`tests/record_test.rs::test_varint_all_lengths`). Requirement-level
  `**Tests:**` lines are only a fallback pool. `tools/assurance.py`
  validates both file AND `::symbol` existence — a link to a missing test
  or missing test function is reported as a DEAD LINK and does not count.
- **PR review checks the dashboard.** `make assurance` before and after: a
  feature PR should move Completeness and/or Scenarios-backed, and must not
  introduce dead links. Spec 005's maintenance rule applies (#25).
- **Tier stubs flip on close.** `tests/tiers/tier{0..3}.rs` (spec 001 Tier
  Model, #69) are executable, claim-oriented contracts alongside the
  evidence-oriented corpus harness. Every feature ticket's acceptance
  criteria include un-`#[ignore]`ing the tier stub(s) it discharges — a
  stub's `#[ignore = "..."]` reason names the V-block/phase/ticket that
  flips it. `tools/assurance.py`'s `Tier contracts:` line (active/total per
  tier) should move when a ticket lands; `tier0.rs` itself must never gain
  an `#[ignore]` — it's the never-droppable gate.

## Grammar conventions

- **Grammar source of truth:** `.openspec/grammar/sqlite.ebnf` — an EBNF
  re-derivation of SQLite's `parse.y` (pinned at 3.53.4; SQLite publishes
  no EBNF). Parser work starts from this file, never from memory or
  third-party grammars.
- **Grow the grammar in the same PR.** Any ticket that extends the parser
  extends `sqlite.ebnf` in the same PR: new rules carry a V-block tag
  (`(* V2 *)`, `(* V3 *)`, …) and a `[parse.y:LINE rulename]` origin
  annotation. Future-block rules stay listed as stubs so the coverage
  denominator remains visible.
- **Run `make grammar-drift` before committing grammar changes.**
  `tools/grammar_drift.py` validates every annotation against the pinned
  parse.y (rule exists, cited line within ±5) and reports per-V-block
  coverage. Drift (unknown rule, stale line citation) is a spec bug — fix
  the annotation, don't loosen the tolerance. Bumping the parse.y pin
  (`SQLITE_VERSION` in the tool) is a deliberate, reviewed change, like an
  oracle bump.

## Module layout conventions

- **No `mod.rs` files.** Use the modern per-file module style — `foo.rs`
  next to `foo/` — instead of `foo/mod.rs`. Tracked by #73.
- Enforced two ways: `self_named_module_files = "deny"` in
  `[lints.clippy]` (Cargo.toml) catches it under `make lint`, and a
  `mod-files` Makefile gate is a dependency-free backstop for anyone
  running gates without clippy.
- Genuinely vendored third-party source under `tests/spike/` (e.g.
  `tests/spike/.../lemon-rs/third_party/lemon`, the C lemon tool itself)
  is exempt — not ours to restructure. Our own hand-authored spike code
  (even inside a `.../lemon-rs/` directory) follows the same no-`mod.rs`
  convention as `src/`. The gate itself only scans `src/`, so this is a
  style rule for spike code, not a `make lint` failure.
