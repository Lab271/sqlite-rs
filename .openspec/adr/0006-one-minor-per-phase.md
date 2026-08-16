# 0006 — Versioning: one minor per completed plan phase

**Status:** Accepted · **Date:** 2026-08-14

## Context

Early iterations released per merge: 0.4.0–0.6.0 appeared with phase 4's deliverable shipping *between* two halves of phase 3's locking work — version numbers had decoupled from value. Only v0.1.0 was ever tagged and the crate is unpublished, so renumbering was free at that moment and impossible later.

## Decision

**One minor version per completed plan phase**; sub-steps stay inside a phase. A phase's version ships only when every ticket in its epic checklist is closed. The 0.4–0.6 range was renumbered into the scheme (history note in CHANGELOG). `VERSION_MAP` in tools/assurance.py encodes minor → block/phase/epic; the dashboard's Model line reports released and in-flight phases.

## Alternatives rejected

- Release-per-merge (the observed drift).
- Calendar versioning (says nothing about capability).

## Consequences

The version number tells the plan's story; 0.5.0 shipping with phase 1 still open was caught and documented as a policy violation, and 0.6.0's ship condition was written explicitly. Extending VERSION_MAP is part of creating each new epic.
