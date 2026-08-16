# 0010 — Deterministic inventories over estimates

**Status:** Accepted · **Date:** 2026-08-15

## Context

Scope stated as estimates drifts: plan.md said "~40 core opcodes" for V2; the oracle-EXPLAIN harvest found 57 (40% over), including whole opcode families hidden behind single words ("Sort").

## Decision

Wherever scope can be **statically enumerated, enumerate it and commit the artifact**; the inventory is simultaneously the scope definition and the completeness checklist. Instances: `tools/opcodes-v2.json` (harvested from pinned-oracle EXPLAIN, frozen by the phase opener); `.openspec/grammar/sqlite.ebnf` (V-block-annotated productions, drift-checked against parse.y line-by-line); fixture families (spec 004); MC/DC condition obligations (the rust-mcdc design, mvl-rust#85).

## Alternatives rejected

- Prose estimates (the 40% miss).
- Inventories as documentation only (they must be machine-checked: grammar-drift gate, dashboard opcode-completeness line — an unchecked inventory rots like any doc).

## Consequences

The assurance dashboard's Model section reports coverage against these denominators. Re-running a harvest is a deliberate scope event (phase opener), not a side effect.
