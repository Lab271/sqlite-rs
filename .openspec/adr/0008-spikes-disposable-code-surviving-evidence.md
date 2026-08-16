# 0008 — Spike discipline: disposable code, surviving evidence

**Status:** Accepted · **Date:** 2026-08-15

## Context

Repeatedly, a helpful artifact (an end-to-end file reader, a CSV exporter, a tree-walking evaluator) would accelerate learning but would diverge from the target architecture if kept.

## Decision

Such artifacts are built as **spikes**: timeboxed (1–3 days), explicitly throwaway (`tests/spike/NNN_*`, frozen after close), with falsification criteria stated up front ("the spike FAILS valuably if…"). **The code is disposed; the evidence survives**: findings.md feeds ticket specs, and test material (e.g. spike 008's oracle-diffed expression vectors) is committed as acceptance corpus for the real implementation — the ratchet. Promoting spike code to production is a plan change requiring an ADR.

## Alternatives rejected

- Prototyping in production modules (divergence pressure, review burden).
- No spikes / spec-first everything (V1's estimate errors — "~40 opcodes" — show why empirical scouting beats guessing).

## Consequences

Eight spikes ran ahead of their phases (parser toolchain, file read, CSV, WAL, locking, grammar slice, opcode harvest, tree walker); every one's findings went into ticket specs before implementation. Spike dirs are exempt from mvl-limit and never refactored.
