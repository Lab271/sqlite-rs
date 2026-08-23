# Spike 010: `rust-refine` (mvl-lang/mvl-rust) proof-of-concept — findings

Issue #371. Branch `spike/010_mvl_refinements`. This directory is the
disposition of that issue: a self-contained crate (`Cargo.toml`,
`src/lib.rs`, `tests/runtime_enforcement.rs`) that reproduces every
finding below independently — run `make test` (or `cargo build`,
`cargo test`, `cargo mvl prove src/lib.rs` directly; `make help` lists
all targets) right here, no dependency on the rest of `sqlite-rs` — plus
this findings doc. See "Disposition" at the end for why the main package
carries none of this. For the short-form verdict, see
[`README.md`](./README.md); this file is the detailed, round-by-round
evidence behind it.

## Scope

This is an experiment in two things, not one: **does `rust-refine`
work**, and **does adopting it make sense for `sqlite-rs`**. Evaluated
against a real, `impl`-heavy, `Result`-returning parsing/validation
function shaped like `sqlite-rs`'s actual `src/header.rs`
(`DatabaseHeader::parse`, `DatabaseHeader::usable_page_size`) — five
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

## Round 5 — mvl-rust `d875d92` (v0.7.1)

Filed #113/#114/#115 (round 4's three gaps) directly against
`mvl-lang/mvl-rust`. All three fixed **in one PR**
([mvl-lang/mvl-rust#116](https://github.com/mvl-lang/mvl-rust/pull/116),
`fix: rust-refine solver follow-ups from sqlite-rs spike (#113, #114,
#115)`), released as v0.7.1 the same day they were filed. Bumped the pin
and reran the *same, unchanged* repro functions from round 4 — this is
the biggest jump of any round:

- **#113 (cast expressions) confirmed fixed — for bare parameters only.**
  `cast_on_bare_param_also_blocked` now closes at **L4** (`src/lib.rs`).
  But `PageWithNarrowField::usable_page_size_with_cast` — the identical
  cast on a **field projection**, exactly `header.rs`'s real
  `self.reserved_space as u32` shape — is unchanged, still `runtime`.
  #113 fixed casts on the bare-parameter path specifically, not the
  general case. `DatabaseHeader::usable_page_size` still has to delegate
  to the free `compute_usable_page_size` for this reason.
- **#114 (`||` short-circuit propagation) confirmed fixed, and this is
  the one that actually matters for `parse`.** Every early-`Err`-return
  obligation in both `known_shape_fold_not_propagated_through_or` and
  `DatabaseHeader::parse` itself now closes — `ensures#0` in the former
  at **L2**, and **3 of `parse`'s 4** return-site obligations
  (`ensures#0`, `#1`, `#2` — every early `return Err(...)`) now close at
  **L1**. This is a real, direct improvement on the issue's original
  target: three-quarters of `parse`'s obligations that were `runtime`
  in round 4 are proven in round 5, with **zero code changes** — the fix
  landed in the tool, not in the annotation.
- **The 4th obligation — `parse`'s `Ok(...)` return, the actual
  field-validation postcondition (`page_size.is_power_of_two()` etc.) —
  is still `runtime`.** Isolated why with
  `known_shape_fold_not_propagated_through_or`'s own `Ok(x)` obligation
  (also still `runtime`, corrected during this round — the original
  predicate here was accidentally false at `x = 0`, a bug in the spike's
  own test, not a tool finding): #97/#114 fold the outer
  `is_ok`/`is_err` call and propagate that fold through `||`, but
  neither resolves `result.as_ref().unwrap()` on a known-shape `Ok(x)`
  back to `x` itself for further reasoning. That's a distinct,
  not-yet-addressed step — see "The bigger unlock" below.

**Go/no-go: upgraded — the tool now proves most of what round 4 could
only assert.** `parse`'s early-return obligations (the majority of its
surface area) are genuine compile-time proofs as of v0.7.1. What's left
runtime-only is now narrowed to exactly the field-validation success
case, which is squarely `mvl-lang/mvl-rust#110`'s territory (see
below) — not a new, separately-discovered gap.

## Gaps fixed in v0.7.1 (round 5)

All three of round 4's gaps, filed against `mvl-lang/mvl-rust`, fixed
same-day in one PR
([mvl-lang/mvl-rust#116](https://github.com/mvl-lang/mvl-rust/pull/116)):

1. **[#113](https://github.com/mvl-lang/mvl-rust/issues/113) — cast
   expressions block solver variable-binding.** Fixed for the
   bare-parameter case (`cast_on_bare_param_also_blocked` now closes at
   L4) — see the narrower residual gap below, this was not a complete
   fix.
2. **[#114](https://github.com/mvl-lang/mvl-rust/issues/114) — `||`
   doesn't propagate a folded-true branch past an unprovable one.**
   Fixed — this is the one that mattered most for `parse`: 3 of its 4
   return-site obligations now close at L1, with no annotation changes.
3. **[#115](https://github.com/mvl-lang/mvl-rust/issues/115) — `pub mod
   { ... }` hides return-site/`impl` scanning.** Filed but not
   independently re-verified this round (this crate's `src/lib.rs` stays
   flat regardless, so nothing here depends on it) — worth confirming
   directly if a future round revisits module-organized code.

## Gaps remaining, narrower than round 4 found them

1. **Cast on a field projection still blocks variable-binding — #113
   was a partial fix.** `PageWithNarrowField::usable_page_size_with_cast`
   (`src/lib.rs`, `self.reserved_space as u32`) stays `runtime` even
   though the identical cast on a bare parameter
   (`cast_on_bare_param_also_blocked`) now closes at L4. Not yet filed
   as its own follow-up issue — `#113`'s fix evidently covers the
   bare-identifier path but not the field-projection path #95 added
   alongside it. This is exactly why `DatabaseHeader::usable_page_size`
   still has to delegate to a free function.
2. **`.unwrap()` on a known-shape `Ok(x)`/`Some(x)` doesn't resolve to
   `x` for further reasoning.** `#97`'s fold and `#114`'s propagation
   together prove `result.is_err()`/`result.is_ok()` themselves, but
   neither unwraps a syntactically known `Ok(x)` to let `x`'s own
   properties (branch-narrowed bounds, or in `parse`'s real case, struct
   fields) enter the proof. This is `DatabaseHeader::parse`'s last
   remaining obligation (the actual field-validation postcondition) and
   `known_shape_fold_not_propagated_through_or`'s `Ok(x)` case — both
   still `runtime`. Not a new discovery this round; it's the same
   "method-call reasoning" limitation below, just now isolated as the
   *only* thing left blocking `parse`.

## The bigger unlock: method-call reasoning (in progress upstream)

Both gaps above are the same root limitation, now narrowed to its
essential case: `DatabaseHeader::parse`'s real postcondition needs
`is_power_of_two()` and an unwrapped struct's fields to participate in a
proof, and the native L1-L4 solver treats arbitrary method calls and
value-carrying constructors as opaque by design (ADR-0001) — #97/#114
proved that *shape-level* facts (is this `Ok` or `Err`?) can fold and
propagate, but not the *payload*. Independently of this spike,
[mvl-lang/mvl-rust#110](https://github.com/mvl-lang/mvl-rust/issues/110)
(implementing
[ADR-0011](https://github.com/mvl-lang/mvl-rust/blob/main/.openspec/adr/0011-resolved-pure-closure-licence.md),
designed in
[#103](https://github.com/mvl-lang/mvl-rust/issues/103)) is in progress:
a sound purity licence letting a same-file, side-effect-free call
participate in a proof instead of being opaque. If it lands, it's the
thing to re-test `DatabaseHeader::parse`'s actual postcondition against
next — see the README's "Next steps".

## Disposition

Per the `spike/DDD_xxxxx` convention (`CLAUDE.md`), this is a disposable
experiment: `sqlite-rs`'s actual `src/header.rs`, `Cargo.toml`,
`deny.toml`, and `.github/workflows/ci.yml` carry **none** of the `mvl`
dependency, annotations, or CI wiring explored across rounds 1-5 above —
those were mistakenly committed directly to production files in the
original attempt (PRs #374, #393, #400, #401, #405) instead of being
isolated here from the start, and have been reverted. This directory is
the sole trace of the spike; `.openspec/` carries no reference to it.
Should `rust-refine` adopt the fixes suggested above and someone wants
to re-evaluate broader adoption, this crate is the starting point — bump
the `mvl` rev in `Cargo.toml` and rerun.
