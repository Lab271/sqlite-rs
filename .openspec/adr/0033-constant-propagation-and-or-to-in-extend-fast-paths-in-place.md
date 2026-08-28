# 0033 — Constant propagation and OR-to-IN extend existing equality fast paths in place; only genuine range seeks wait for a new opcode

**Status:** Accepted · **Date:** 2026-08-28 · **Revised:** 2026-08-28

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

**Revision (same day):** OR→IN conversion turned out not to need a new
opcode either. Initial analysis assumed it needed a genuine multi-value
seek instruction. On implementation, a simpler shape sufficed: an
OR-chain of *pure equalities* against the same column is a finite list of
point values, and each point value is already exactly what
`SeekRowid`/`SeekIndexEq` probe for. `try_compile_rowid_seek` and
`try_compile_covering_index_scan` now loop over the resolved operand list
— one `SeekRowid`/`SeekIndexEq` per value, chained so a miss (wrong value,
or the index/table exhausted) falls through to the next operand's own
fresh seek, and the last operand's miss falls through to `end_label`
exactly as the single-operand case always did. No new opcode, no new
control-flow primitive — just the existing seek opcodes, repeated.
Landed as spec `012-query-constraints` Requirement 2, same PR as
constant propagation.

This does **not** generalize to #606: a genuine *range* (`LIKE` prefix,
`BETWEEN`, non-literal `IN`-list-from-subquery) has no finite enumerable
point-value list to loop over — that family still needs a real
range-seek opcode, growing the frozen inventory (`009-vdbe-codegen`,
`tools/opcodes-v2.json`). That work stays out of scope here and is
tracked as spec `012-query-constraints` Requirement 3 (#606).

**A new spec (012), not folding into 011.** `011-analyze-cost-model`
covers statistics-driven cost estimation between already-recognized
scan/probe options; it explicitly assumes the "purely structural
pattern-matching" of `choose_join_access` stays as-is. Constraint
extraction — deciding *whether* a fast path is even eligible in the first
place — is a different concern from costing between eligible ones, so it
gets its own spec home.

## Alternatives rejected

- **Keep constant propagation and OR→IN as separate PRs, per the original
  split.** The split was made before either was implemented, on the
  (mistaken) assumption OR→IN needed new opcode work. Once implementation
  showed it didn't — it's the same "loop over resolved operands, reuse
  the existing single-key seek" shape constant propagation already
  established — there was no remaining reason to keep them apart, and
  they landed in the same PR.
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

- `try_compile_rowid_seek` and `try_compile_covering_index_scan` (via
  `find_covering_index`) each gained both a `propagate_constants`
  fallback and an `or_chain_equality_operands` fallback; `find_skip_scan_index`
  only gained the former, and `choose_join_access` (join ON-clause
  equality) got neither — #605's acceptance criteria only asked for
  single-table WHERE-clause propagation/conversion. Skip-scan's OR-chain
  case is a genuinely different shape (checking membership in a value
  *set* per index entry during a single walk, not repeated fresh seeks)
  and join-level propagation raises its own correctness questions (which
  side of the join a column belongs to) — both are separate tickets if
  wanted later.
- `is_rowid_reference` was split into an `Expr`-based wrapper plus a
  name-based `is_rowid_reference_name`, since the propagated map is keyed
  by column name, not by AST node.
- `CoveringIndexMatch.operand: Expr` became `.operands: Vec<Expr>` (always
  non-empty) to carry either the single-equality/propagated case or an
  OR-chain's multiple values through one shape; `try_compile_rowid_seek`
  builds its own local `Vec<Expr>` the same way rather than changing a
  shared struct, since it has no `eqp.rs`-reporting counterpart to keep in
  sync.
- Future Requirement 3 work (#606's LIKE/BETWEEN/IN ranges) will need its
  own range representation — the `Vec<Expr>` operand-list shape here is
  specific to "finite enumerable point values" and does not generalize to
  an open range; that's a separate design question when #606 is picked
  up.
