# 0039: `Value`'s text and blob payloads are `Arc`, not `Rc`

Date: 2026-09-04

## Context

`Value::Text(Rc<str>)` and `Value::Blob(Rc<[u8]>)` made `Value` itself
`!Send`. A result row is a `Vec<Value>`, so no query result could cross a
thread boundary at all.

That blocks spec 013's embedding API outright. Its Requirement 4 asks for a
`Send + Sync` connection handle, and ADR-0034 (as filed in PR #678) proposes a
worker thread that "creates its `Rc` graph on a thread it owns and never lets
it leave". Rows are precisely the thing that must leave, so the proposed design
is not sufficient as written — a gap neither the spec nor that ADR records,
because both locate the `Send` problem in the pager alone.

**The obvious objection is that ADR-0013 and ADR-0017 already rejected `Arc`.
They did not — not for this.** Both are about the *pager*: one `Vm` shares a
page source across N cursors via cheap `Rc` clones, and making that `Arc` would
tax the Tier 0 read path to serve a requirement that lives at the connection
boundary. `Value`'s payloads were never their subject. The natural reading of
those two ADRs is nevertheless "`Arc` anywhere is settled against", which is
why this needs writing down rather than being left as an apparent
contradiction.

Spike 014 (#682) measured the change instead of arguing it:

- **+22/−17 across 6 files.** Nearly every construction site writes
  `Value::Text(s.to_string().into())`, and `.into()` is identical for `Rc<str>`
  and `Arc<str>`, so only the ~12 sites that *name* the type needed editing.
- **1562 tests pass, 0 fail** — identical to the `Rc` baseline.
- **No measurable read-path cost.** `full_drain/batch` is single-threaded with
  no thread boundary, so it isolates the tax: `Rc` runs spanned 5.42–5.62 ms,
  `Arc` runs 5.24–5.33 ms. One comparison reported `Arc` 5.6% *faster*, which
  is not a credible speedup from adding atomics — the honest reading is that
  the difference sits inside run-to-run variance.
- It removes the boundary copy permanently: handing rows over untouched ran
  5.13 ms against 7.34 ms for the owned-copy alternative.

## Decision

`Value::Text` and `Value::Blob` hold `Arc<str>` and `Arc<[u8]>`. `Value` is
therefore `Send + Sync`, enforced by a `const` assertion in
`src/record/value.rs` rather than by convention.

`Rc<dyn PageSource>` and `Rc<RefCell<Pager>>` are **unchanged**. ADR-0013 and
ADR-0017 remain in force on exactly the question they decided.

## Alternatives rejected

- **An owned copy at the API boundary** (`String`/`Vec<u8>`), leaving `Value`
  as `Rc`. Works, and was the spike's first prototype — but it costs an
  allocation and copy per text and blob value on every row, forever, and it
  puts two value types in front of consumers: the engine's and the API's.
  Measured 7.34 ms against 5.13 ms on a 50,000-row drain. Kept as the fallback
  if the `Arc` change is ever judged too invasive on principle, since the
  measured case against it is the only case against it.
- **Handing back encoded record bytes** and making the consumer decode. No
  per-value copies, but it moves the record format into the public API and
  pushes decoding onto every consumer.
- **`Arc` for the pager too**, unifying the story. Rejected: that is the change
  ADR-0013 and ADR-0017 actually considered and refused, and nothing here
  disturbs their reasoning. The read path shares a page source across cursors
  on one thread; the connection boundary is a different problem with a
  different answer.

## Consequences

- `Value` is `Send + Sync` and consumers may move rows between threads. The
  embedding API's worker-thread design (spec 013/Req 4) becomes implementable
  without a second value type.
- Cloning a `Value` now costs an atomic increment rather than a non-atomic one.
  Measured as unobservable on the read path, but the spike's noise floor is a
  few percent, so **this should be re-measured on `tests/performance/engine.rs`
  against the pinned oracle before the figure is quoted anywhere as settled.**
- ADR-0034 (PR #678) needs amending or superseding on its Req 4 rationale: the
  `Send` obstacle is not only the pager. That is #678's to fix, not this ADR's.
- Anything that reconstructs a `Value` from a shared buffer now needs `Arc`
  semantics; `src/record/decode.rs` and the sorter/hash-aggregation paths were
  the only such sites and are updated here.
- ADR numbering is contended across three unmerged branches: `feat/683` claims
  0038, this claims 0039, and PR #678's claims whatever is free at its merge.
  Numbers are only settled by merge order, so the last to land renumbers.
