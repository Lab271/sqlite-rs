# 0013 — Scalar subqueries out of V2; aggregates arrive with V4

**Status:** Accepted · **Date:** 2026-08-15

## Context

The first opcode harvest accidentally included `WHERE id = (SELECT max(id) …)`, dragging AggStep/AggFinal/BeginSubrtn/Return into a block whose plan says "no aggregates until V4." The grammar EBNF already marks subqueries V4.

## Decision

Scalar subqueries (and with them all aggregate machinery) are **out of V2 scope**. The harvest set was corrected and re-frozen by the phase-3 opener (#87): 52 opcodes, no Agg*. Aggregates arrive in V4 together with GROUP BY, where they are implemented once, properly.

## Alternatives rejected

- Folding AggStep/AggFinal into V2 "because the harvest showed them" (scope creep by measurement accident; aggregates without GROUP BY machinery would be rework at V4).

## Consequences

V2's demo query class is honest: single-table, no subqueries. The grammar's `IN (select)` and `(select)` stubs stay V4-tagged; the parser's three-way diagnostics report them as "not yet supported," distinct from syntax errors.
