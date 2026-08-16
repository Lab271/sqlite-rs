# 0012 — Ephemeral tables: opcode semantics preserved, in-memory backing

**Status:** Accepted · **Date:** 2026-08-15

## Context

The opcode harvest showed DISTINCT and ORDER BY+LIMIT compile to a full ephemeral-table machine (OpenEphemeral/Sequence/IdxInsert/Found/Delete — a b-tree-backed dedup/top-K path), not flags on ResultRow. Stock SQLite backs ephemerals with real (temp) b-tree pages.

## Decision

Implement the ephemeral opcode family with **in-memory ordered storage (BTreeMap keyed by kernel comparison)**. The opcodes' observable semantics — the compatibility contract (ADR-0004) — are preserved exactly; the backing store never touches the file format or temp files.

## Alternatives rejected

- On-disk ephemeral b-trees (drags the write path into V2; format surface for zero compatibility gain — ephemerals are never visible to another process).
- Descoping DISTINCT from V2 (it is in the grammar slice and the demo; and V4 reuses the machinery for compound selects and IN-subqueries).

## Consequences

V2 phase 3B ships DISTINCT and top-K ORDER BY+LIMIT without any temp-file management. Revisit only if memory ceilings appear at V4 scale (spill-to-disk would then be an extension, not a rewrite).
