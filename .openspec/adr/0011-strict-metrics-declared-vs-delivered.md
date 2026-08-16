# 0011 — Strict metrics: declared is not delivered

**Status:** Accepted · **Date:** 2026-08-14

## Context

The assurance dashboard initially reported 48% scenario coverage with zero lines of code — it counted **Tests:** links declared in specs without checking the files existed. After phase 1 landed, the same flaw inverted: 21% under-reporting from stale links while 32 real tests went uncounted. An intermediate "Linked" state proved to be pure noise.

## Decision

Two metrics only, both requiring artifacts on disk: **Completeness** (requirement → implementation file exists) and **Coverage** (scenario → test file AND `::symbol` exist), plus one error signal (**DEAD LINKS** — a declared link that does not resolve is a spec bug to fix, never partial credit). `(planned)` marks future work, excluded from scoring entirely. A link to an unwritten test is a plan, not evidence.

## Alternatives rejected

- Counting declarations (both failure directions observed within 24 hours).
- Three-state scoring (declared/linked/verified): the middle state answers no question anyone asks.

## Consequences

Zero code reads 0% everywhere. The same fix was filed upstream against the mvl tool the dashboard was ported from (mvl-lang/mvl#2284). Per-scenario test links became the closing convention for every feature ticket.
