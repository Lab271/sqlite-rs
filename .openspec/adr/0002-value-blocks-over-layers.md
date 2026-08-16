# 0002 — Value blocks over layer-ordered development

**Status:** Accepted · **Date:** 2026-08-13

## Context

The first development plan was layer-ordered (tokenizer → parser → AST → VFS → pager → b-tree → VDBE → codegen → API): correct dependency order, but nothing usable until nearly everything exists.

## Decision

Replace it with twelve **value blocks** (V1–V12), each delivering usable capability — "go from working to working." Grammar, layers, and corpus are sliced per block, never built wholesale. V1 reads existing files with no SQL engine at all.

## Alternatives rejected

- **Layer order:** no demonstrable value for months; risk retired late.
- **Feature-parallel tracks:** unbounded WIP, no phase gates.

## Consequences

Every phase boundary is a defensible product (V1 reader, V4 engine, V6 drop-in). Versioning, epics, tier tests, and parity suites all inherit the block structure. Cost: some layers are visited repeatedly (pager in V1/V3/V5/V6).
