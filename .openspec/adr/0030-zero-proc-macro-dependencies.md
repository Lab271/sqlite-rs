# 0030 — Zero proc-macro dependencies: hand-rolled errors and readline

**Status:** Accepted · **Date:** 2026-08-26

## Context

MVL supply-chain principles (documented in `DEPENDENCIES.md`) treat every
dependency as a trust boundary, and proc-macro dependencies as the worst
case within that: they execute arbitrary code at build time, not just at
run time. Two production dependencies carried transitive proc-macro cost:

- `thiserror` — derive-macro error enums across 15 modules (~101 variants:
  `header.rs`, `dump.rs`, `vfs.rs`, `vfs/page_source.rs`, `record/error.rs`,
  `codegen/dispatch.rs`, `codegen/select.rs`, `vdbe/exec.rs`,
  `vdbe/functions.rs`, `schema/ddl_reader.rs`, `btree/error.rs`,
  `pager/wal.rs`, `pager/error.rs`, `pager/journal.rs`,
  `pager/freelist.rs`).
- `rustyline` — the CLI shell's line editor, history, and completion.

Both are widely-used, reputable crates; the decision is not about their
quality but about narrowing the trust surface for security-sensitive
contexts sqlite-rs targets (forensics, FRANK-style analysis pipelines),
where a build-time code-execution dependency is a strictly larger risk
than a runtime-only one.

## Decision

- #553 (PR #559): hand-write `Display`/`Error`/`From` impls for all 15
  `thiserror`-derived enums, preserving `#[error("...")]` message text
  byte-for-byte and `#[source]`/`#[from]` chaining semantics, then drop the
  `thiserror` dependency.
- #558 (PR #562): replace `rustyline` with a hand-rolled readline
  (`src/bin/sqlite-rs/readline/`) — line editing, in-memory history with
  XDG-path persistence, SQL-aware tab completion, and tokenizer-backed
  syntax highlighting — depending only on `nix` for raw-mode termios.

Production code now has zero proc-macro dependencies (verified per
`DEPENDENCIES.md`).

## Alternatives rejected

- Keeping `thiserror`/`rustyline` and accepting the proc-macro trust
  surface — rejected per MVL principles once a zero-proc-macro posture was
  judged achievable at reasonable hand-rolling cost (~101 error variants;
  ~1050 lines for readline).
- Fencing proc-macro usage behind a feature flag or dev-only boundary —
  not applicable; both crates were used unconditionally in the shipped
  binary/library.

## Consequences

- `cargo geiger` / `cargo mvl` reports zero proc-macro deps in production
  code; this ADR is the durable record of *why*, since `DEPENDENCIES.md`
  documents the current dependency set but not decision history.
- `.openspec/plan.md`'s dependency list still names `thiserror` — stale as
  of this ADR; update in the same PR that adopts this document.
- Any future dependency addition that pulls in a proc macro must be
  weighed against this precedent, not added silently.
