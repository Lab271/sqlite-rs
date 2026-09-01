# 0038: One streaming execution primitive, with the batch path as its wrapper

Date: 2026-09-01

## Context

#683: every execution entry point (`execute`, `execute_with_params`,
`execute_with_db`, `execute_with_db_and_params`, `execute_with_writable_db`,
`execute_transaction_step`) returns a fully materialized
`Vec<Vec<Value>>`. Reading row 0 of a large result therefore costs building
row N, and there is no way for a caller to stop early without having already
paid for everything.

That blocks an embedding API. Spec 013/Req-7 (PR #678) asks for incremental row
access, and a `Statement::next_row` cannot be layered on top of a function that
has already collected every row before returning.

It was also not implementable outside the crate. `fn dispatch`
(`src/vdbe/exec.rs`) and `fn run` are private, and `ResultRow` does not hand a
row to anyone — it calls `Vm::emit_row`, which pushes into a `Vec` *inside* the
`Vm`. An external driver had no way to advance the program one instruction at a
time, and no way to run `run`'s `Halt`-0 arm, which performs the implicit
commit.

Spike 014 (#682) prototyped and measured the alternatives on a 1,000,000-row
result:

| | peak heap | time to first row |
|---|---:|---:|
| batch (`execute_with_db`) | 137.7 MB | 5.36 ms |
| streaming | 8.68 MB | 44.7 µs |

Streaming's peak scales 1.03x for 2.5x the rows read, because it is bounded by
`DEFAULT_PAGE_CACHE_CAPACITY` rather than by result size. Batch scales 2.20x
and is unbounded in principle.

## Decision

Add one public primitive, `Execution`, holding the program-loop state (`vm`,
`program`, `pc`, `steps`, `done`, `pending`) and exposing `new`, `next_row` and
`autocommit`. **Reimplement `run()` as a wrapper that collects `next_row` into
the same `Vec` it already returned.**

The wrapper is the load-bearing half of this decision. Batch and streaming are
then literally the same loop, so a behavioural difference between them is a bug
in one rather than a divergence callers must reason about — and the existing
suite becomes the equivalence proof: 1562 tests pass with identical per-binary
counts before and after.

`pending` is a FIFO, not a pop off the back. `Vm::emit_row` has three callers,
and `pragma::integrity_check` emits *one row per problem found* from a single
dispatch. Draining from the back would silently reverse
`PRAGMA integrity_check` output, and every other opcode emits at most one row,
so nothing else would look wrong. This is not hypothetical: no test in the
suite reached the multi-row path before #683, which is why
`vdbe_streaming_execution_test.rs` corrupts three indexes to force it and why
that test was mutation-checked against a pop-off-the-back implementation.

## Alternatives rejected

- **Two parallel execution paths** — keep `run` as it is and add a separate
  streaming loop beside it. Rejected: two copies of the step limit, the
  program-counter bounds check and the `Halt`-0 implicit-commit arm, drifting
  independently, with no test able to prove they agree. The wrapper shape costs
  one `Vec` push per row and makes drift impossible.
- **Making `dispatch` public and letting callers write their own loop.**
  Rejected: `run`'s `Halt`-0 arm flushes the writer when `autocommit` is set,
  and `vm.db`/`vm.autocommit` are private, so an external loop would silently
  skip the implicit commit. The bug would surface as lost writes, not as a
  compile error.
- **Returning an `Iterator`** instead of a `next_row` method. Rejected for now:
  `Iterator::next` cannot return a borrow of the `Execution`, and the error
  type makes `Item = Result<Vec<Value>, ExecError>` the only shape, which
  forces every consumer through `collect::<Result<_,_>>()` or a manual loop
  anyway. `next_row` keeps the error in the caller's control flow. An
  `Iterator` adapter can be added over this without changing it.
- **Streaming at the transport level instead** — keep materializing inside the
  engine and chunk on the way out. Rejected: it cannot fix peak heap, which is
  the 137.7 MB figure above. The engine has to stop accumulating.

## Consequences

- `Execution` is public API and subject to the crate's stability policy from
  here. Its scope is deliberately narrow: it does not own the pager, the
  header, parameter binding or the transaction flag.
- **Only read-only statements can be streamed from outside the crate today.**
  `Vm::autocommit` is private and `execute_transaction_step` sets it
  internally, so an external caller can build a streaming `Vm` via
  `Vm::with_db` but cannot thread the transaction flag. That is sufficient for
  result-producing statements, which is Req-7's target, and a
  transaction-aware constructor is deferred to whichever ticket needs it rather
  than speculatively added here.
- **A facade built on this must chunk its transport.** #682 measured a
  one-row-per-message channel at ~4.5 µs per row — 39.9x slower than batch on a
  full drain — and chunking at ~1024 rows erasing the penalty entirely (7.30 ms
  against a worker-batch baseline of 7.43 ms). `Execution` itself has no
  channel and no chunk size; this constrains the facade, not the primitive.
- The memory bound is the page cache, not zero. Any acceptance criterion
  phrased as "without materializing" should be stated as
  `min(pages_touched, DEFAULT_PAGE_CACHE_CAPACITY) x page_size`, independent of
  result size.
- Spec 013's Req-4 rationale needs correcting separately: `Value::Text(Rc<str>)`
  and `Value::Blob(Rc<[u8]>)` make `Value` itself `!Send`, so a worker-thread
  design pays an owned copy per text and blob value at the boundary. That is
  ADR-0034's territory (as filed in PR #678), not this ADR's.
