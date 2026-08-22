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

## Go/no-go

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

Re-evaluate once `visit_impl_item_fn` lands upstream; Finding 2 would
then be the next thing to check.
