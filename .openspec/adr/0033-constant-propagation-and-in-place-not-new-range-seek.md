# 0033 — Constant propagation extends existing equality fast paths in place; range/OR-to-IN seeks wait for a new opcode

**Status:** Accepted · **Date:** 2026-08-28

## Context

#605 asked for two related optimizations: constant propagation
(`a = b AND b = 5` deducing `a = 5` for index use) and OR→IN conversion
(`x = 1 OR x = 2 OR x = 3` → a multi-value seek). Investigating the
codebase first (see #605/#606 issue comments) found no existing
WHERE-clause constraint-extraction pass at all — every seek/probe fast
path (`try_compile_rowid_seek`, `find_covering_index`,
`find_skip_scan_index`, `choose_join_access`) recognizes only a single
top-level `column = <literal|param>` equality via
`top_level_equality_operands`, and bails on any `AND`/`OR` compound
condition. Both halves of #605, plus #606's LIKE/BETWEEN/IN range work,
turned out to be greenfield rather than an extension of partial support.

## Decision

**Split #605 into two phases, and ship only constant propagation now.**

Constant propagation needs nothing beyond what already exists: it only
ever resolves a column to a literal/bind-parameter operand the existing
equality fast paths already know how to consume. So `propagate_constants`
(`src/codegen/select/limit_scan.rs`) walks the top-level `AND`-conjunction
of a WHERE/ON clause, builds a `column name -> resolved constant` map via
direct equalities and transitive `column = column` chains, and each
existing fast path now checks this map instead of (or in addition to) the
single-equality check it had before. No new opcode, no new spec-level
constraint model — the map is fed straight into machinery that already
exists.

OR→IN conversion is different in kind: it needs a genuine multi-value
seek, which does not exist in the VDBE today (only single-key equality
seeks: `SeekRowid`, `SeekIndexEq`, `AutoIndexSeek`). Building that means
growing the frozen opcode inventory (`009-vdbe-codegen`,
`tools/opcodes-v2.json`) — a deliberate, reviewed change, not something to
fold into the same PR as a purely-additive map lookup. It is deferred to
a follow-up ticket, tracked as spec `012-query-constraints` Requirement 2,
alongside #606's LIKE/BETWEEN/IN range work (Requirement 3), which needs
the same new opcode.

**A new spec (012), not folding into 011.** `011-analyze-cost-model`
covers statistics-driven cost estimation between already-recognized
scan/probe options; it explicitly assumes the "purely structural
pattern-matching" of `choose_join_access` stays as-is. Constraint
extraction — deciding *whether* a fast path is even eligible in the first
place — is a different concern from costing between eligible ones, so it
gets its own spec home.

## Alternatives rejected

- **Land constant propagation and OR→IN together, since the issue bundled
  them.** Rejected: OR→IN's opcode work is substantial and independent of
  the map-lookup change; bundling them would have blocked a safe,
  self-contained improvement behind a much larger one.
- **A general `IndexConstraint`/range-constraint struct now, sized for
  future range/OR-to-IN work too.** Rejected as premature — nothing today
  needs a range representation (constant propagation only ever produces
  point values), so a struct built ahead of that need would be
  speculative. The map-of-literals shape is exactly what today's
  requirement calls for; a range-constraint model is designed when
  Requirement 2/3's actual seek shape is worked out.
- **Extend `top_level_equality_operands` itself to recurse into `AND`.**
  Rejected: every call site currently destructures its `(&Expr, &Expr)`
  return directly against `is_rowid_reference`/`where_col`, which assumes
  literal AST nodes, not resolved values. A parallel `propagate_constants`
  keeps the existing function's contract (and its narrow #137 callers)
  untouched, and gives every caller a plain `HashMap` lookup instead of
  re-deriving AST-shape checks against propagated results.

## Consequences

- `try_compile_rowid_seek`, `find_covering_index`, and
  `find_skip_scan_index` each gained a `propagate_constants` fallback
  path; `choose_join_access` (join ON-clause equality) was left alone —
  #605's acceptance criteria only asked for single-table WHERE-clause
  propagation, and join-level propagation (correctness across which side
  of the join a column belongs to) is enough of a separate question to
  warrant its own ticket if wanted later.
- `is_rowid_reference` was split into an `Expr`-based wrapper plus a
  name-based `is_rowid_reference_name`, since the propagated map is keyed
  by column name, not by AST node.
- Future Requirement 2/3 work (OR→IN, LIKE/BETWEEN/IN ranges) will most
  likely want to generalize the current `HashMap<String, Expr>` point-value
  map into something that can also carry a range — at that point revisit
  whether `propagate_constants`'s shape still fits or needs its own
  ADR-documented redesign.
