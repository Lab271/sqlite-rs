# 0014 — Expression nesting bound stays at 200, not SQLite's 1000

**Status:** Accepted · **Date:** 2026-08-16

## Context

`MAX_EXPR_DEPTH` (`src/parser/grammar.rs`) guards the expression parser's
recursion so pathological nesting returns a clean `ParseFail::Invalid`
instead of overflowing the stack. Real sqlite3's equivalent,
`SQLITE_MAX_EXPR_DEPTH`, defaults to 1000.

#118 found the guard was unreachable in a debug build in the first place: a
2 MiB thread (libtest's default) overflowed the stack around 61 levels of
`abs(...)` nesting — before the counter, at three guard checkpoints per
level (`expr`/`not_expr`/`unary_expr`), ever reached its 200-checkpoint
(~67-level) limit. The fix collapsed two groups of pure pass-through
precedence levels — `or_expr`+`and_expr`, and
`relational_expr`/`bitwise_expr`/`additive_expr`/`multiplicative_expr`/
`concat_expr` — into two precedence-climbing functions (`bool_expr`,
`binary_expr`), each one stack frame instead of five (or two). This was
chosen over `#[inline(always)]` on the original ladder: measured, forcing
that attribute made per-level cost *worse*, not better — merging large
multi-branch functions (`equality_expr` in particular) into one unoptimized
-O0 frame bloats total local-variable space rather than sharing it, unlike
a real reduction in call count.

Measured after the fix, on a 2 MiB thread, debug build:

| input nesting levels | outcome |
|---|---|
| 61 | Accepted |
| 75 | Accepted |
| 79 | stack overflow (guard raised past 200 to isolate the true ceiling) |
| 100–5000 (guard at its real 200) | `Invalid`, guard fires cleanly |

The real stack-safety ceiling moved from ~61 to ~75–79 levels — a genuine
but modest ~25% improvement, not the order-of-magnitude the ladder-collapse
alone might suggest, because `primary_expr`/`function_call`/`expr_list`
(the frames a function-call argument recurses through) are untouched and
dominate the remaining per-level cost. `MAX_EXPR_DEPTH=200` (~67 real
levels) now fires safely below that ~75–79 ceiling — which is the actual
fix: the guard is reachable, where before it was not.

## Decision

Keep `MAX_EXPR_DEPTH` at 200 (~67 real nesting levels). Do not raise it
toward SQLite's 1000.

Reaching 1000 real levels would need roughly `1000 / 75 * 2 MiB ≈ 27 MiB`
of stack per parse in a debug build — the same order of magnitude as the
16 MiB mitigation #118 set out to shrink, just relocated from a test-only
thread override into every caller's runtime requirement. A divergence
already existed before this ticket (61 accepted vs. SQLite's 1000); this
keeps it, narrows the debug-build risk that made it urgent, and records it
as deliberate rather than accidental.

## Alternatives rejected

- **Raise `MAX_EXPR_DEPTH` to ~3000 (1000 real levels via the 3
  checkpoints/level ratio).** Rejected: not safely reachable within a
  default-size stack post-fix (see measurements above); would reintroduce
  the same class of bug this ticket exists to close, just at a higher
  threshold.
- **`#[inline(always)]` on the full precedence ladder**, per the issue's
  first suggested approach. Rejected on measurement: it made debug-build
  stack cost per level dramatically *worse* (a 5-level nesting test that
  passed on the unmodified ladder aborted after blanket inlining), because
  forced inlining of large multi-branch functions at `-O0` bloats combined
  stack-frame size rather than sharing it across mutually-exclusive
  branches the way an optimizing build would.
- **Full rewrite to an explicit iterative/heap-based expression parser**
  (eliminating the remaining `primary_expr`/`function_call`/`expr_list`
  frames too). Would likely close the gap to SQLite's 1000 for real, but is
  a materially larger change than this ticket's scope — a candidate for a
  follow-up ticket if 1000-level parity is ever required.

## Consequences

- `with_parser_stack`'s 16 MiB thread override in
  `tests/corpus/extracted_sql_test.rs` is removed entirely (not just
  shrunk) — all three tests that used it now run on the default test
  thread, since the guard fires safely below the new overflow ceiling.
- The divergence from SQLite's `SQLITE_MAX_EXPR_DEPTH=1000` is intentional
  and documented here, per the qualified-subset/dialect-divergence
  convention (ADR-0004); revisit only alongside the iterative-rewrite
  alternative above, not by incrementally nudging the constant.
