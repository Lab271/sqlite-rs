# Spike 010: `rust-refine` (mvl-lang/mvl-rust) proof-of-concept — findings

Issue #371. Branch `spike/010_mvl_refinements`. This directory is the
disposition of that issue: a self-contained crate (`Cargo.toml`,
`src/lib.rs`, `tests/runtime_enforcement.rs`) that reproduces every
finding below independently — run `make test` (or `cargo build`,
`cargo test`, `cargo mvl prove src/lib.rs` directly; `make help` lists
all targets) right here, no dependency on the rest of `sqlite-rs` — plus
this findings doc. See "Disposition" at the end for why the main package
carries none of this.

## Scope

Evaluate `rust-refine` — one of the five `mvl-lang/mvl-rust` tools —
against a real, `impl`-heavy, `Result`-returning parsing/validation
function shaped like `sqlite-rs`'s actual `src/header.rs`
(`DatabaseHeader::parse`, `DatabaseHeader::usable_page_size`). Four
rounds, each bumping the `mvl` pin to the latest fix and re-verifying
concretely rather than trusting a closed-issue title.

## Round 1 — mvl-rust `c765404` (v0.4.0)

The issue's target annotation:

```rust
#[mvl::refine]
impl DatabaseHeader {
    #[requires(buf.len() >= HEADER_LEN)]
    #[ensures(ret.is_ok() ==> ret.page_size.is_power_of_two())]
    ...
}
```

isn't real `rust-refine` syntax at all: no `#[mvl::refine]` impl-block
wrapper attribute exists; `#[requires]`/`#[ensures]` need the `mvl::`
qualifier; the return value is always named `result`, not `ret`; and
`==>` isn't a Rust operator the predicate's `syn::Expr` parser accepts
(implication has to be spelled `!p || q`). This is a documentation/
API-understanding gap on the issue's part, not a tool defect.

**Blocking finding:** `rust-refine`'s scanner
(`crates/rust-refine/src/checks.rs`) implemented `Visit::visit_item_fn`
only — no `visit_impl_item_fn` anywhere in the `mvl-rust` workspace.
`cargo mvl prove` against a correctly-annotated, compiling `impl` method
returned `"obligations": []` at **exit code 0** — silent success by
omission, not an error. A free function with the identical attribute was
picked up correctly. Since real-world parsing/validation code lives in
`impl` blocks almost exclusively, this made `rust-refine` unable to
check any of `header.rs`'s actual invariants.

**Go/no-go: no-go.** Posted as a comment on sqlite-rs#371; picked up and
fixed same-day as mvl-lang/mvl-rust#90 (`fix(refine): check impl
methods, not just free functions`, released v0.4.2), citing this finding
directly.

## Round 2 — mvl-rust `1d3b8ef` (v0.4.2)

Confirmed #90 fixed: `impl` methods are now scanned, both
declaration-site and one obligation per return site.

**New finding:** a `Result<T, E>`-returning function's `ensures`, when
it references a field of the `Ok` payload (`result.as_ref().unwrap()
.page_size`), fails to *compile* at an early `return Err(...)` site with
`E0282 "type annotations needed"` — `Err(...)` alone doesn't pin `T`
early enough for the generated `{ let result = Err(...); assert!(...);
result }` block to type-check. This blocked expressing the issue's
actual target postcondition at all; the annotation had to be reduced to
a tautology (`result.is_ok() || result.is_err()`) to compile.

**Go/no-go: conditional-go, blocked on this finding.** Posted with a
minimal repro on mvl-lang/mvl-rust#90's PR comments; fixed same-day as
mvl-lang/mvl-rust#93 (closing #92, `fix(mvl-macros): pin result's type
at every ensures return-site`), released v0.5.1.

## Round 3 — mvl-rust `0fe5f55` (v0.5.1)

Confirmed #93 fixed: the full field-level postcondition from the
issue's original target now compiles and is enforced at every return
site. `unit_header`-equivalent tests passed with the *real* invariant
live, not a tautology.

Every obligation still discharged at `layer: "runtime"`, not proven
statically. Tried extracting the pure-arithmetic `usable_page_size` into
a free function over bare `u32` params
(`compute_usable_page_size`, `src/lib.rs:101`), on the theory that
`self.field` access alone was the blocker. That theory was wrong on its
own — the free function *also* fell to `runtime`. Isolating further
found **two independent causes**:

1. **No implicit unsigned lower bound.** The solver reasons over
   unbounded integers and never infers a `u32` parameter's `>= 0` for
   free — `reserved_space <= page_size` alone doesn't give
   Fourier-Motzkin enough to derive `page_size - reserved_space <=
   page_size`. Restating `reserved_space >= 0 && page_size >= 0`
   explicitly made the return-site `ensures` close, at **L4**.
2. **`self.field` still doesn't reach the solver as a variable at
   all**, confirmed separately: the identical logic and identical
   (now-sufficient) bounds, restated on `self.field` instead of a bare
   parameter, still fell to `runtime`.

**Go/no-go: go, with one proven data point.** Filed as
mvl-lang/mvl-rust#94 (implicit bound) and #95 (field projection);
`compute_usable_page_size`'s `returns::ensures#0` reported `"layer":
"L4"`, `"warrant": "proof"` — the first genuine compile-time proof in
this whole spike. Both issues closed same-day, released v0.7.0.

## Round 4 — mvl-rust `c3ebad8e` (v0.7.0)

Verified #94, #95, and a third fix (#97, filed alongside #94/#95 for the
`(Err(x)).is_err()`-should-fold-at-L1 pattern seen in `parse`'s
predicate) concretely rather than trusting the closed titles:

- **#94 confirmed, and simplified the code**: dropped the explicit
  `>= 0` clauses from `compute_usable_page_size` — still closes at L4
  (`src/lib.rs:101`, mirrored standalone at
  `implicit_unsigned_bound_now_injected`, `src/lib.rs:124`).
- **#95 confirmed, but narrower than hoped**: a bare `self.field`
  repro with no cast now closes at L4 with the `>= 0` bounds still
  stated explicitly (`Page::usable_page_size_field_projection`,
  `src/lib.rs:153`) — #94's automatic injection doesn't extend to field
  projections, only bare parameters. And critically for `header.rs`'s
  actual shape: **an `as` cast blocks it again**, regardless of whether
  the cast wraps a field projection
  (`PageWithNarrowField::usable_page_size_with_cast`, `src/lib.rs:181`)
  or a bare parameter (`cast_on_bare_param_also_blocked`,
  `src/lib.rs:196`) — confirmed both ways, both still `runtime`.
- **#97 confirmed in isolation, but doesn't reach `parse`'s real
  predicate**: `Err(x).is_err()` alone now folds to a literal `bool` at
  L1/L2 (`known_shape_fold_in_isolation`, `src/lib.rs:210`). But the
  fold doesn't propagate through `||` to short-circuit a combined
  expression when the second clause still contains something out of
  #97's scope (`known_shape_fold_not_propagated_through_or`,
  `src/lib.rs:224`, mirroring `DatabaseHeader::parse`'s actual
  `result.is_err() || (...)` shape) — stays `runtime`.

A fifth, unrelated limitation surfaced while *building* this spike
crate, not from testing `header.rs` itself: wrapping the annotated items
in a `pub mod { ... }` block hides return-site obligations and `impl`
methods from `cargo mvl prove` entirely — declaration-site free-function
obligations are the only thing that still gets found through a module.
This crate's `src/lib.rs` is deliberately flat (no `mod` wrappers) to
avoid it; see "Open gaps" below.

**Go/no-go: unchanged from round 3 in substance** — still one proven L4
data point, `parse`'s real invariant still `runtime`-only, now for a
third independently-confirmed reason.

## Open gaps for a future `mvl-lang/mvl-rust` agent

Everything below is reproducible directly in this crate — run
`cargo mvl prove src/lib.rs` and compare against the `Expected:` line in
each function's doc comment. None of these are filed as GitHub issues
yet.

1. **Cast expressions block solver variable-binding.**
   `PageWithNarrowField::usable_page_size_with_cast` (`src/lib.rs:181`)
   and `cast_on_bare_param_also_blocked` (`src/lib.rs:196`) both stay at
   `runtime` despite sufficient, explicit bounds — an `as` cast around
   either a field projection or a bare parameter isn't recognized as a
   bindable variable by the interval/Fourier-Motzkin solver. Likely
   fix shape: extend whatever identity-extraction logic #94/#95 added
   (bare identifiers, then field projections) to also see through a
   single `Expr::Cast` to the identifier/projection underneath, treating
   the cast as a widening (`u8 -> u32`) that doesn't change the value's
   provable bounds. This is the more actionable of the two gaps here —
   real code casting a narrower unsigned field before combining it with
   a wider one (exactly `header.rs`'s `reserved_space as u32`) is common.
2. **`||` doesn't propagate a folded-true branch past an unprovable
   one.** `known_shape_fold_not_propagated_through_or` (`src/lib.rs:224`)
   shows `result.is_err() || <unprovable>` staying at `runtime` even
   though `result.is_err()` alone (see
   `known_shape_fold_in_isolation`, `src/lib.rs:210`) folds to `true` on
   the exact same `Err(...)` shape. Likely fix shape: after folding a
   `MethodCall` per #97, apply ordinary boolean short-circuit
   simplification (`true || x => true`, `false && x => false`) at the
   same L1 pass, rather than only folding the leaf and stopping. This
   is the one that would actually make `DatabaseHeader::parse`-shaped
   functions (the common case: validate-then-construct, with the
   postcondition checking the successful case's fields) provable rather
   than perpetually `runtime`.
3. **`pub mod { ... }` hides return-site/`impl` scanning.** Not
   triggered by anything in `header.rs` (which is a flat file), but hit
   while writing this spike's first draft: a free function's
   declaration-site `requires`/`ensures` are found correctly through a
   `mod`, but its return-site obligations are not generated at all, and
   an `impl` block nested in a `mod` is invisible entirely — not even a
   declaration-site obligation. Likely same root cause as #90's original
   `impl`-invisibility bug (a `Visit` override or item-collection pass
   that iterates `file.items` without recursing into `Item::Mod`'s own
   `content`). Lower priority than 1-2 since it doesn't block anything
   in this specific codebase, but worth fixing for any crate that
   organizes real code into modules (i.e. most of them).

## Disposition

Per the `spike/DDD_xxxxx` convention (`CLAUDE.md`), this is a disposable
experiment: `sqlite-rs`'s actual `src/header.rs`, `Cargo.toml`,
`deny.toml`, and `.github/workflows/ci.yml` carry **none** of the `mvl`
dependency, annotations, or CI wiring explored across rounds 1-4 above —
those were mistakenly committed directly to production files in the
original attempt (PRs #374, #393, #400, #401, #405) instead of being
isolated here from the start, and have been reverted. This directory is
the sole trace of the spike; `.openspec/` carries no reference to it.
Should `rust-refine` adopt the fixes suggested above and someone wants
to re-evaluate broader adoption, this crate is the starting point — bump
the `mvl` rev in `Cargo.toml` and rerun.
