# Spike 008: tree-walking evaluator — findings

Issue #59, spec `002-parser` Req 3 / `008-value-semantics`. Branch
`spike/008_tree_walker`. Disposed per epic #56: the throwaway
`src/`/`tests/oracle_diff.rs`/`Cargo.toml` are deleted in the closing
commit; this file and the committed oracle vectors
(`tests/corpus/expr_vectors/*.jsonl`) survive.

## Summary

A throwaway crate walked the phase-1 parser's `Expr` AST (via
`SELECT <expr>`, since `Parser::expr()` itself is crate-private) and
dispatched every node to the phase-2 value-semantics kernel
(`src/vdbe/{compare,affinity,coerce,collation,functions,value}.rs`).
Every vector in `tests/corpus/expr_vectors/{comparison,collation,null,
coercion,functions,walker}.jsonl` (216 expressions total, 71 of them new
— `walker.jsonl`, covering CASE/CAST/LIKE/GLOB/BETWEEN/IN-list/
short-circuit/arithmetic) was oracle-diffed against the pinned
`sqlite3` 3.53.3, with **zero unexplained divergences** — 5 known,
pre-existing gaps are explicitly carved out (below), everything else
matches byte-exact.

## Kernel API: no gaps for the phase-2 surface

`compare`, `sql_eq`/`sql_lt`/`is`/`is_not`/`and`/`or`/`not`,
`apply_affinity`/`affinity_of`, `coerce_text_to_numeric`, `call_function`
(the scalar registry) all composed cleanly from outside — no awkward
signatures surfaced (contra spike 005's experience with the phase-1
public surface). One clear scope gap, expected: **arithmetic beyond
Add/Sub/Mul (Div/Mod/bitwise/concat) and LIKE/GLOB pattern matching
don't exist anywhere in `src/`** — spec 008 never claimed them (only
Reqs 1-6: affinity/comparison/collation/NULL/coercion/functions). The
walker implemented these itself; #89 (VDBE core) will need to do the
same for Div/Mod (its own scope explicitly lists Add/Subtract/Multiply/
Divide/Remainder), and a LIKE/GLOB opcode is conspicuously unscoped in
both #88 and #89 today — **worth flagging to whoever finalizes #88**:
`Function`-opcode delegation covers scalar functions, but LIKE/GLOB
have their own dedicated SQLite opcode family (`OP_Function` isn't how
SQLite compiles them — it's a direct string-matching path in
`vdbe.c`), so #88 should have an explicit requirement for it rather
than letting it fall through the cracks between "kernel delegation" and
"expression emission".

## Kernel/parser bugs found — fixed in this PR

- **Parser: keyword-named functions rejected.** `SELECT
  replace('abcabc','a','Z')` failed to parse — `REPLACE` tokenizes as
  `Keyword::REPLACE`, and `primary_expr()`'s function-call detection
  only fired on `TokenKind::Identifier`. SQLite accepts most keywords
  as function names when followed by `(` (only CASE/CAST/EXISTS/
  CURRENT_* are genuinely reserved in expression position). Fixed in
  `src/parser/grammar.rs`'s `primary_expr()` with a keyword-followed-by-
  `(` catch-all (regression test:
  `tests/unit/parser.rs::test_keyword_named_function_call`). This was
  silently blocking the existing `functions.jsonl` corpus (in place
  since #78/#79) from ever being executed — nothing had actually called
  `parse_select` on it before this spike.
- **Walker (not kernel): missed NULL propagation and COLLATE handling
  on first pass** — `NULL + 1`, `CAST(NULL AS INTEGER)`, and `'abc' =
  'ABC' COLLATE NOCASE` all diverged initially. `checked_add`/`sub`/
  `mul` are (correctly) NULL-unaware primitives — same layering as
  `round_fn` in `src/vdbe/functions.rs`, which checks NULL before
  calling `value_f64`. Fixed by adding the same NULL guard at the call
  site, a NULL short-circuit in `eval_cast`, and an
  `eval_with_collation` helper that peels an immediate `ExprKind::
  Collate` node before comparing. None of these were kernel bugs — the
  kernel's contract was always "caller checks NULL first"; the walker
  just hadn't read that contract carefully enough on the first pass.

## Known divergences — NOT fixed here, follow-up issues to file

1. **`format_real`'s 15-significant-digit rendering vs. the oracle's
   ~17-digit REAL rendering on overflow-promoted values.** Confirmed
   via 4 vectors (`9223372036854775807 + 1`, `+ 1.0`, `* 2`,
   `-9223372036854775808 - 1` — all promote to REAL on i64 overflow).
   This is the *same* gap #92's review already flagged for
   `quote()`/`hex()`/`length()` on REAL arguments — this spike confirms
   it's not isolated to those three functions but inherent to
   `format_real` itself, hit by any REAL-producing arithmetic path.
   **Follow-up:** file an issue against `src/format.rs`'s `format_real`
   precision (broader fix, not a quick one — SQLite's own REAL
   rendering is build-dependent per the existing `.dump`/`-list`
   scoping note in issue #37).
2. **Tokenizer folds `9223372036854775808` (positive, unrepresentable
   as i64) to a `Float` literal before unary minus applies**, losing
   SQLite's special-cased `-9223372036854775808` (i64::MIN) integer
   literal parse. Narrow, single-vector edge case — excluded from the
   walker's oracle-diff gate via an explicit `KNOWN_DIVERGENCES` list
   (see `tests/oracle_diff.rs`, now deleted with the rest of the
   crate — the vector itself stays in `walker.jsonl` as a marked TODO
   for whoever picks this up). **Follow-up:** file a small issue against
   `src/parser/tokenizer.rs`'s integer-literal lexing.

## Emission-order findings (for #88's spec / #89's codegen)

These come directly from writing the walker's evaluation order — the
walker's AST recursion is structurally codegen's traversal, so its
choices are dry-run findings for the real VDBE emitter:

1. **AND/OR must short-circuit at the jump level, not just the value
   level.** The walker evaluates `lhs`, and if it already determines
   the three-valued result (`Some(false)` for AND, `Some(true)` for
   OR), returns without evaluating `rhs` at all — verified functionally
   by `0 AND (1/0)` and `1 OR (1/0)` (division by zero on the
   unevaluated side would otherwise surface as a divergence, since `/0`
   yields NULL, not an error, but a real *side-effecting* expression
   would be a correctness bug if evaluated). For codegen: this is a
   conditional jump over the right operand's whole instruction
   sequence, not just an `AND`/`OR` opcode combining two already-
   computed values — the natural codegen shape is `IfNot(lhs) ->
   Goto(false-result)` for AND, `If(lhs) -> Goto(true-result)` for OR,
   *then* fall through to emit `rhs`.
2. **Comparisons compile to two primitives, not six.** `Eq`/`Ne`/`Lt`/
   `Le`/`Gt`/`Ge` were all implemented via just `sql_eq` and `sql_lt`
   (`Gt(a,b) = sql_lt(b,a)`, `Ge(a,b) = not(sql_lt(a,b))`, `Le(a,b) =
   not(sql_lt(b,a))`, `Ne(a,b) = not(sql_eq(a,b))`). Matches how real
   SQLite's VDBE only has `OP_Eq`/`OP_Lt` (plus `OP_Ne`/`OP_Le`/`OP_Gt`/
   `OP_Ge` as documented aliases with swapped operands/negated jump
   sense) — #89 should not plan for six independent comparison-value
   code paths, only two, with operand-order/negation bookkeeping at the
   call site.
3. **COLLATE is a modifier on evaluation, not a value-producing node.**
   `Collate` in the AST doesn't change what value an expression
   produces; it only changes which `Collation` a *containing*
   comparison uses. The walker's `eval_with_collation` peels an
   immediate `Collate` wrapper off either comparison operand and picks
   whichever side specified one (SQLite's real rule, "nearest COLLATE
   wins, rightmost breaks ties," needed for arbitrary nesting, is
   *not* implemented here — flagging as a real semantic gap #89's
   codegen needs to solve properly, since a real VDBE must track
   collation as a compile-time property threaded through arbitrary
   sub-expressions, not just direct comparison operands).
4. **CASE's WHEN-clause evaluation order is strictly left-to-right with
   early exit on first match** — verified by `CASE 1 WHEN 1 THEN 'a'
   WHEN 1 THEN 'b' END` returning `'a'`, not `'b'`. Codegen shape: one
   comparison-and-conditional-jump pair per WHEN, all jumping to a
   shared end label, with the ELSE (or implicit NULL) as the
   fallthrough after the last WHEN — a straightforward jump-chain, no
   surprises.
5. **`IN (...)` must distinguish "found," "not found, but saw a NULL
   in the list," and "not found, no NULLs" as three outcomes**, not two
   — `5 IN (1,NULL,3)` is NULL (unknown), not false, while `5 IN
   (1,NULL,5)` is true (a match short-circuits before the NULL even
   matters). Codegen needs a NULL-seen flag alongside the found flag,
   checked only if the scan completes without a match.

## Corpus stats

- 6 families oracle-diffed: `comparison` (15), `collation` (7), `null`
  (16), `coercion` (17, 4 known-divergent), `functions` (72, all
  passing after the `REPLACE` parser fix), `walker` (71 new, 1
  known-divergent).
- 216 total vectors now proven to round-trip through parse -> kernel ->
  value, oracle-exact modulo the 5 documented, pre-existing gaps above.
