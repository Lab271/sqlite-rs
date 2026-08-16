# 0007 — Expressions compile to opcodes; semantics live in a kernel; no evaluator

**Status:** Accepted · **Date:** 2026-08-15

## Context

The first V2 proposal had a standalone tree-walking expression evaluator (phase 2) consumed by the VDBE (phase 3). But SQLite has no evaluator: `expr.c` emits opcodes, WHERE is control flow, and the risky semantics (affinity, comparison, collation, NULL rules) already live as pure functions beneath the opcodes (`vdbemem.c`/`func.c`).

## Decision

SQLite-faithful: expressions compile to VDBE opcodes; all value semantics live in one pure **value-semantics kernel** (`src/vdbe/{value,affinity,coerce,collation,compare}.rs`, spec 008) that opcodes delegate to. One semantics implementation, ever. The VDBE is a dumb dispatcher owning only control flow, registers, and cursor plumbing.

## Alternatives rejected

- **Standalone evaluator kept in production:** two sources of semantic truth, architectural divergence from the system being replicated, EXPLAIN never comparable — the exact bug class oracle-diffing exists to prevent, self-inflicted.
- **Evaluator inside opcodes without a kernel:** semantics smeared across the dispatch, untestable standalone; affinity (the riskiest corner) needs standalone retirement.

## Consequences

Phase 2 became the kernel (0.6.0), de-risking affinity before any opcode existed. The tree-walker survives only as disposable spike 008 (ADR-0008). Promoting any walker to production requires a superseding ADR.
