# 0004 — The compatibility contract: format + dialect + locking, not internals

**Status:** Accepted · **Date:** 2026-08-13

## Context

Binary compatibility needed a precise definition. Two late insights sharpened it: (1) SQLite has no server — OS advisory file locking IS the concurrency infrastructure, and a stock sqlite3 process will open our files live; (2) `sqlite3 -csv` etc. live in a third artifact, the CLI shell (~30K lines), which is also our oracle's interface.

## Decision

The contract has three surfaces: **file format** (static bytes, incl. WAL/journal companions), **SQL dialect** (accept what SQLite accepts, reject what it rejects), and **locking protocol** (fcntl byte ranges, `-shm` slot protocol — format-right but locking-wrong yields silent corruption only visible under concurrent access). Explicitly OUT of contract: VDBE bytecode, query plans, internal algorithms (recorded informationally in the parity suite, never gated). Shell output parity (`-list`/`-csv`) is adopted for our own tools because it makes oracle diffs normalization-free; full shell/dot-command parity is a non-goal (possible V13).

## Consequences

Spike 005 validated locking against a live sqlite3; safe-reader locking landed in V1. Parity suite (tests/parity/) gates acceptance/output/schema, loosely gates plans, never gates VM streams. Our codegen is free to differ from SQLite's where output is identical.
