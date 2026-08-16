# 0005 — Pinned non-codec sqlite3 as the sole correctness authority

**Status:** Accepted · **Date:** 2026-08-14

## Context

We do not define SQLite correctness — SQLite does. Spike 002 found macOS's system sqlite3 is a codec build (see-cccrypt, 12 reserved bytes/page) silently producing different files than stock SQLite.

## Decision

All fixture generation and behavioral diffs run against a **pinned, non-codec sqlite3 build** whose version is recorded once (Cargo.toml metadata) and asserted loudly by every harness (corpus, parity, benches, rusqlite tier-1). Oracle version bumps are deliberate, reviewed changes — like dependency bumps. Both reserved-byte cases (0 and 12) stay in the corpus as free edge coverage.

## Alternatives rejected

- System sqlite3 (the codec trap, silent drift per machine).
- Self-defined expected outputs (we would be grading our own homework).
- Multiple oracle versions (divergence noise without added authority).

## Consequences

Every claim in the repo bottoms out in a byte-level diff against one known binary. The oracle is also the reference implementation the disposable spikes diff against (ADR-0008), removing any need for a second in-tree engine.
