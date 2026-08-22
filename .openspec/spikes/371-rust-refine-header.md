# Spike #371: rust-refine proof-of-concept on `header.rs`

## Setup

Path dependency on a local `mvl-rust` checkout (`crates/mvl`), annotated
`DatabaseHeader::parse` and `DatabaseHeader::usable_page_size` in
`src/header.rs` with `#[mvl::requires]`/`#[mvl::ensures]`, ran
`cargo mvl refine`/`cargo mvl prove` against the file, ran the existing
`unit_header` test suite with the runtime-assertion injection active.

## Issue's target syntax doesn't exist

The issue specified:

```rust
#[mvl::refine]
impl DatabaseHeader {
    #[requires(buf.len() >= HEADER_LEN)]
    #[ensures(ret.is_ok() ==> ret.page_size.is_power_of_two())]
    ...
}
```

None of `#[mvl::refine]` (impl-block wrapper), bare `#[requires]`/
`#[ensures]` (unqualified), `ret` (vs. the real fixed name `result`), or
`==>` (implication — not a Rust operator, and the predicate parser only
accepts `syn::Expr` or a bounded quantifier) are real. The actual API is
per-function `#[mvl::requires(pred)]`/`#[mvl::ensures(pred)]`, `result`
names the return value, and implication has to be spelled `!p || q`.
This is a documentation/API-understanding gap on the issue's part, not a
tool defect — noted here for the record, not filed upstream.

## Finding 1 (blocking): `impl` methods are invisible to the scanner

`rust-refine`'s scanner (`crates/rust-refine/src/checks.rs`) implements
`Visit::visit_item_fn` only — there is no `visit_impl_item_fn`. Confirmed
workspace-wide: no crate under `mvl-rust/crates/` (`rust-refine`,
`rust-total`, etc.) implements it either.

Consequence, confirmed directly: `cargo mvl prove src/header.rs` against
`DatabaseHeader::parse`/`usable_page_size` — both correctly annotated,
both compiling — returns `"obligations": []` and **exit code 0**. A free
function with the identical attribute (`/tmp/free_fn_test.rs`) is picked
up correctly. So this isn't a parse error or a rejected attribute; the
checker silently never looks at `impl` bodies at all, and reports success
by omission rather than failing loud.

This is the load-bearing finding for `header.rs` specifically: every
target in the issue (`DatabaseHeader::parse`) is an inherent `impl`
method — the SQLite codebase's parsing/validation logic overwhelmingly
lives in `impl` blocks, not free functions. As shipped, `rust-refine`
cannot statically check any of it, and gives no diagnostic saying so.

## Finding 2: early-return sites can break type inference for
`ensures`

Separately from Finding 1 — reproduced on a free function shaped like
`parse` (`Result<T, E>` return with multiple early `return Err(...)`
sites) before Finding 1 was understood. `ensures` is injected at *every*
return point (by design, ADR-0006 §5 condition 1 — better than upstream's
tail-only instrumentation). But referencing a field of the `Ok` variant
directly, e.g. `result.as_ref().unwrap().page_size >= 512`, fails to
compile at an early `return Err(...)` site with **E0282 "type annotations
needed"**: `Err(...)` alone doesn't pin the `Ok` type `T`, and the
generated `{ let result = Err(...); assert!(result.as_ref().unwrap()...);
return result; }` block apparently doesn't get that pinned early enough
for the assert's expression to type-check.

Workaround found (not applied to the final annotation per review
feedback — kept here as the actual gap statement): routing through a
helper function whose signature names the concrete `Result<DatabaseHeader,
HeaderError>` (`fn parse_postcondition(result: &Result<DatabaseHeader,
HeaderError>) -> bool`) pins `T` immediately and compiles. This means
struct-field postconditions on a `Result`-returning function are only
reachable today via an extra helper, not directly in the attribute —
distinct from Finding 1 and worth its own upstream issue if `impl`
support (Finding 1) is ever fixed and this becomes the next blocker.

## What the annotations that did compile actually prove

- `DatabaseHeader::parse`'s `ensures` had to be reduced to a tautology
  (`result.is_ok() || result.is_err()`) to compile at all, given Findings
  1 and 2 — proves nothing, and (per Finding 1) isn't even scanned.
- `usable_page_size`'s `requires`/`ensures` (`self.reserved_space as u32
  <= self.page_size` / `result <= self.page_size`) compile fine — no
  early returns, no `Result` — but per Finding 1 are equally invisible to
  `cargo mvl prove`/`refine`.
- Runtime enforcement (the `assert!` injection) works regardless of
  Finding 1 — confirmed the `unit_header` test suite (5 tests) still
  passes with both functions' assertions live at every call.

## Go/no-go (first attempt)

**No-go for broader adoption on `sqlite-rs`, as of this checkout
(`mvl-rust` commit `c765404`, workspace version 0.4.0).** The one thing
this codebase's invariants need — checking methods inside `impl` blocks —
is entirely unimplemented in the scanner, and the tool reports success
(exit 0, empty obligation list) rather than an error when pointed at
`impl`-only code, which is the worst failure mode for something meant to
be an assurance signal. Runtime enforcement (the `assert!` injection)
does work on `impl` methods and could be adopted independently for
regression-catching, but that's `rust-total`/plain `debug_assert!`
territory, not what this ticket was evaluating.

Not filed as a formal upstream issue — this finding, posted as a comment
on sqlite-rs#371, was picked up directly and fixed via
[mvl-lang/mvl-rust#90]. `rust-total`/`rust-effect` share the identical
root cause (`Item::Fn`-only scanning) and are tracked separately by
[mvl-lang/mvl-rust#89], not fixed by #90.

## Retry, against mvl-rust v0.4.2 (mvl-lang/mvl-rust#90)

Upstream fixed Finding 1 within the day: `1d3b8ef` (PR #90, `fix(refine):
check impl methods, not just free functions`, released as v0.4.2) adds
`visit_impl_item_fn` (`impl_methods()`, `FnFacts::of_method()`,
`find_method_declarations()`) to the scanner, citing this spike's #371
finding directly in its PR description. Bumped `sqlite-rs`'s
pin — `Cargo.toml`'s `mvl` dependency and `.github/workflows/ci.yml`'s
`MVL_RUST_REV`, both from `a64eb33e` to `1d3b8ef4668a4...` — reinstalled
`cargo-mvl` at the new rev, and reran against the same `header.rs`.

**Finding 1 confirmed fixed.** `cargo mvl prove src/header.rs` now
returns 11 obligations covering both `DatabaseHeader::parse` (one
declaration-site + one per return point, including every early
`return Err(...)`) and `usable_page_size` (`requires`, `ensures`,
return-site) — no longer silently empty. `impl` methods are visible to
the scanner.

**Finding 2 still reproduces, unchanged.** Re-tried the field-level
postcondition the issue actually wanted —
`result.as_ref().unwrap().page_size.is_power_of_two() && ...` — on
`parse`'s `ensures`. Identical `E0282 "type annotations needed"` at the
first early `return Err(...)` site, byte-for-byte the same diagnostic as
the pre-fix attempt. This is unrelated to scanning (it's a `mvl-macros`
codegen/type-inference issue in `inject_ensures`'s return-site
rewriting), so fixing Finding 1 didn't touch it. `parse`'s `ensures`
stays a tautology (`result.is_ok() || result.is_err()`) in the committed
code for the same reason as before.

All obligations that do compile land at `layer: "runtime"` — none prove
statically (`is_power_of_two`, struct construction, and
`page_size - reserved_space` aren't in the linear-arithmetic fragment
`L1`–`L4` reason over) — so this spike hasn't yet exercised a genuine
compile-time proof on real header-parsing code, only confirmed the
obligations are now generated and enforced at runtime.

`unit_header`'s 5 tests still pass with both functions' runtime
assertions live.

## Go/no-go (current)

**Upgraded to conditional-go, blocked on Finding 2.** The blocking gap
from the first pass is resolved upstream and adopted here. What's left
untestable is exactly the postcondition the issue was written for — a
struct-field check on a `Result`-returning parser — because of the
separate `ensures`-at-early-return type-inference bug. Recommend:
surface Finding 2 to `mvl-lang/mvl-rust` next (same channel that
produced #90 — posting it as a sqlite-rs issue comment was enough to get
Finding 1 fixed same-day), and re-run this spike's field-level `ensures`
once it lands.

## Second retry, against mvl-rust v0.5.1 (mvl-lang/mvl-rust#93)

Finding 2 was posted as a comment on [mvl-lang/mvl-rust#90] with a
minimal repro; fixed same-day as [mvl-lang/mvl-rust#93] (`fix(mvl-macros):
pin result's type at every ensures return-site`, closing
[mvl-lang/mvl-rust#92]) — `inject_ensures` now annotates every
instrumented `let result` binding with the function's declared return
type instead of leaving it to inference. Released as v0.5.1 (`0fe5f55`,
which also carries [mvl-lang/mvl-rust#89]'s `impl`-support extension to
`rust-total`/`rust-effect`). Bumped `sqlite-rs`'s pin again, reinstalled
`cargo-mvl`, and restored `parse`'s `ensures` to the full field-level
postcondition from the issue's original target.

**Finding 2 confirmed fixed.** The exact predicate the issue asked for —
`result.as_ref().unwrap().page_size.is_power_of_two() && 512 <=
... <= 65536 && reserved_space < page_size` — now compiles without
E0282, and `cargo mvl prove` returns it as a real obligation at the
declaration site and at all 8 return sites (one per early `return
Err(...)` plus the `Ok` tail). Needed one addition beyond the predicate
itself: `#[allow(clippy::unwrap_used, reason = "...")]` on `parse`, since
this workspace denies `unwrap_used` project-wide and clippy can't see
that the leading `result.is_err() ||` short-circuits the `unwrap()`s
before they'd ever run on an `Err`. `unit_header`'s 5 tests still pass,
now with the *real* invariant enforced at runtime on every call, not a
tautology.

Confirmed unchanged from the first retry: every obligation here —
including this one — still lands at `layer: "runtime"`, per #93's own
scope note ("does not change what rust-refine can prove... the L1–L4
native solver doesn't reason about struct-field access or method calls
like `is_power_of_two()`"). Nothing in this spike has yet produced a
static L1–L4 proof on real header-parsing code; see the "simple L1–L4
addition" discussion below for what would.

## Go/no-go (current)

**Go for runtime-enforcement use; still no static proof exercised.**
Both blocking gaps found by this spike are fixed upstream (same-day, in
both cases) and adopted here: `impl` methods are scanned, and a
`Result`-returning function's `ensures` can inspect the `Ok` payload
from any return site. The issue's original target annotation compiles
and is enforced, verbatim in intent. What `rust-refine` still doesn't
give this codebase is a compile-time *proof* of any of it — `page_size`/
`reserved_space` validation involves `is_power_of_two()` and struct
construction, neither in the linear-arithmetic fragment `L1`–`L4` reason
over, so every obligation here is discharged at `layer: "runtime"`
(equivalent to a hand-written `assert!`, not stronger).

Recommended next step for a real L1–L4 win: extract the pure-arithmetic
core of `usable_page_size` (`page_size - reserved_space`) into a free
function over bare `u32` params rather than `&self` field access — the
interval solver reasons over plain identifiers, not struct-field
projections, so `self.reserved_space`/`self.page_size` are opaque to it
even though the same subtraction the `mask_low_nibble` demo function
proves at L1/L2 for a masking case would very likely close here too. If
that lands, it's worth sweeping the codebase's existing
`#[allow(clippy::arithmetic_side_effects, reason = "...")]` sites —
each is already a hand-written, unenforced precondition claim, and a
free-function `requires`/`ensures` pair could upgrade some of them from
"comment asserting an invariant" to "compiler-checked precondition."

[mvl-lang/mvl-rust#90]: https://github.com/mvl-lang/mvl-rust/pull/90
[mvl-lang/mvl-rust#89]: https://github.com/mvl-lang/mvl-rust/issues/89
[mvl-lang/mvl-rust#92]: https://github.com/mvl-lang/mvl-rust/issues/92
[mvl-lang/mvl-rust#93]: https://github.com/mvl-lang/mvl-rust/pull/93
