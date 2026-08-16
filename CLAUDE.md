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

## Test layout conventions

- **`tests/{corpus,parity,tiers,unit}/` are the only test subdirectories**
  with a defined meaning (oracle-diff corpus, per-V-block oracle parity,
  tier-model contracts, public-API unit tests, respectively — see the
  tier-stub-flip and spec-traceability conventions above).
- **Property tests live under `tests/proptest/`.** `tests/proptest/record_proptest.rs`,
  `tests/proptest/semantics_proptest.rs`, `tests/proptest/tokenizer_proptest.rs`
  are each declared as an explicit `[[test]]` in `Cargo.toml` (subdirectory
  files aren't Cargo-auto-discovered — only direct children of `tests/`
  are, which is why every non-top-level test file, `tests/unit/*`
  included, needs its own `[[test]]` block). Each `[[test]]`'s `name`
  stays the short form (`record_proptest`, not the path), so
  `cargo test --test record_proptest` and the `test-proptest` Makefile
  target are unaffected by where the file physically lives.
  `tests/proptest/proptest-regressions/` sits alongside the property-test
  files themselves — proptest derives that path from the source file's
  own location via `file!()`, so it moves with the files, not
  independently. Spec `Tests:` links use the full current path
  (`tests/proptest/record_proptest.rs::prop_integer_i8_roundtrip`); update
  them in the same PR as any future move so `tools/assurance.py` doesn't
  report DEAD LINKs.

## Epic & phase breakdown conventions

Each value block (`V1`…`V12` in `.openspec/plan.md`) gets one GitHub `epic`-labeled
tracking issue (e.g. #5 for V1, #56 for V2). The epic body — not CLAUDE.md, not
plan.md — is the live source of truth for that block's phase breakdown; keep
these conventions in sync with how #56 is actually structured:

- **One minor version per completed phase, not per block.** A phase's version
  ships only when every ticket in its checklist is closed — a phase in
  progress does not get a version bump partway through. State this
  explicitly in the epic ("0.6.0 requires phase 1 fully closed AND phase 2
  complete"), and encode the block→version mapping in
  `tools/assurance.py`'s `VERSION_MAP` so the dashboard's Model line tracks
  it automatically. If a version ever ships ahead of its phase actually
  closing (it has happened once, #56 phase 1/0.5.0), call that out inline
  in the epic as a one-time exception, not a new pattern.
- **Phase tickets are titled `V{N} phase {M}[{letter}] — <name>`**, e.g. `V2
  phase 2 — value-semantics kernel`, with sub-phases split by trailing
  letter (`V2 phase 3A/3B/3C`) when a phase's work is parallelizable across
  independent tickets that converge at the end (3C explicitly lists what it
  needs from 3A/3B). A phase's final ticket is the deliverable/exit gate
  (`V{N} exit gate — close epic #{epic} at {version}`), which also seeds the
  next value block's epic.
- **Each phase checklist item is a `- [ ]`/`- [x]` line linking the ticket
  number**, with the phase's acceptance gate stated as prose above the list
  (e.g. "Gate: oracle-generated expression vectors pass bit/byte-exact").
  Disposable spikes that feed a phase (per the `spike/DDD_xxxxx` branch
  convention) get their own checklist under a `## Spikes` section, separate
  from shippable phase tickets, so the completeness math isn't polluted by
  throwaway exploration.
- **Cross-reference the other tracked regimes at the bottom.** An epic ends
  with a `## Related regimes` line pointing at the tier-suite ticket (tier
  stubs flip as phases land — see tier-stub-flip convention above), the
  parity-suite ticket (new V-block dimensions activate per phase), and any
  corpus follow-on ticket — so a reader can jump straight to the
  cross-cutting tickets a phase touches instead of re-deriving them.

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


## ADR convention

- **A decision that closes an alternative gets an ADR in the same PR.**
  `.openspec/adr/NNNN-title.md`: Context / Decision / Alternatives rejected /
  Consequences, half a page, dated, indexed in `adr/index.md`. ADRs are
  immutable — supersede with a new one, never edit an accepted one.
- **Check the ADR index before proposing architectural changes** — if your
  proposal contradicts an accepted ADR, the PR must include the superseding
  ADR that argues why the context changed.
- **Uncited-ADR carve-out.** An ADR may still be edited or removed by a
  follow-up PR as long as nothing else in the repo cites it yet — no
  ticket `Refs:` line, no other ADR pointing at it, no spec reference.
  Once something cites it, it's frozen: fix it forward with a superseding
  ADR, never edit in place. This exists so a batch of freshly-authored (or
  backfilled) ADRs can be corrected while they're still wet, without
  spawning a superseding ADR for every rough edge found a day later.
