//! Spike 010: `rust-refine` (mvl-lang/mvl-rust) proof-of-concept, standalone.
//!
//! Issue #371. See `findings.md` in this directory for the full narrative
//! across six rounds of testing against successive `mvl-rust` releases.
//! This crate is the self-contained evidence: every function below
//! compiles against the pinned `mvl` dependency in `Cargo.toml` and can be
//! run through `cargo mvl prove src/lib.rs` to reproduce the exact
//! `layer`/`warrant` reported in `findings.md`, independent of the main
//! `sqlite-rs` package (which carries none of this — see "Disposition" in
//! `findings.md`).
//!
//! Deliberately a flat file, no `pub mod { ... }` wrappers: nested modules
//! turned out to hide return-site obligations and `impl` methods entirely
//! from `cargo mvl prove` (declaration-site free-function obligations are
//! the only thing that still gets found through a module) — a real, fifth
//! solver-scanning limitation found while building this spike, noted in
//! `findings.md` but out of scope to fix here. Everything below is
//! module-free specifically to give an accurate, representative reading.

// ---------------------------------------------------------------------
// Part 1: the corrected, compiling equivalent of the issue's own target.
// ---------------------------------------------------------------------

#[derive(Debug)]
pub enum HeaderError {
    TooShort,
    InvalidPageSize,
    InvalidReservedSpace,
}

/// A best-effort recreation of `sqlite-rs`'s real `src/header.rs`
/// invariants — close enough to reproduce every finding, not a copy of
/// production code (which never carried any of this; see `findings.md`'s
/// "Disposition").
#[derive(Debug, Clone, Copy)]
pub struct DatabaseHeader {
    pub page_size: u32,
    pub reserved_space: u8,
}

impl DatabaseHeader {
    /// The issue's original target, adapted to the real API: `#[mvl::refine]
    /// impl { ... }` with `ret.field` and `==>` aren't real rust-refine
    /// syntax (no impl-block wrapper attribute exists; the return value is
    /// always named `result`; implication has to be spelled `!p || q`
    /// since `==>` isn't a `syn::Expr` the predicate parser accepts). This
    /// is the corrected, compiling equivalent of what the issue asked for.
    ///
    /// Compiles and is picked up by `cargo mvl prove` as of `mvl-rust`
    /// v0.5.1+ (mvl-lang/mvl-rust#90/#93) — findings.md's rounds 1-2. As
    /// of v0.7.1 (mvl-lang/mvl-rust#113/#114/#115, findings.md's round
    /// 5), every early-`Err`-return obligation now closes at **L1**
    /// (`#114`'s `||` short-circuit propagation finishes what `#97`'s
    /// fold started). The one still at `runtime` is the `Ok(...)` return
    /// — the actual field-validation postcondition — because closing it
    /// needs `.unwrap()` to resolve to its wrapped value for further
    /// reasoning (`is_power_of_two()`, comparisons on the unwrapped
    /// fields), which is out of scope for #113/#114/#115 and pending
    /// mvl-lang/mvl-rust#110 (the "resolved-pure closure" purity
    /// licence).
    #[allow(clippy::unwrap_used)]
    #[mvl::ensures(
        result.is_err()
            || (result.as_ref().unwrap().page_size.is_power_of_two()
                && result.as_ref().unwrap().page_size >= 512
                && result.as_ref().unwrap().page_size <= 65536
                && (result.as_ref().unwrap().reserved_space as u32)
                    < result.as_ref().unwrap().page_size)
    )]
    pub fn parse(buf: &[u8]) -> Result<Self, HeaderError> {
        if buf.len() < 100 {
            return Err(HeaderError::TooShort);
        }
        let page_size: u32 = 4096;
        let reserved_space: u8 = 0;
        if page_size < 512 || !page_size.is_power_of_two() {
            return Err(HeaderError::InvalidPageSize);
        }
        if reserved_space as u32 >= page_size {
            return Err(HeaderError::InvalidReservedSpace);
        }
        Ok(DatabaseHeader {
            page_size,
            reserved_space,
        })
    }

    /// Delegates to a free function (`compute_usable_page_size` below)
    /// rather than annotating `&self` directly — see
    /// `usable_page_size_with_cast` further down for why that's still
    /// necessary even after mvl-lang/mvl-rust#95/#113 (a cast on a bare
    /// parameter is fixed; a cast on a field projection, exactly this
    /// method's `self.reserved_space as u32`, is not — round 5).
    pub fn usable_page_size(&self) -> u32 {
        compute_usable_page_size(self.page_size, self.reserved_space as u32)
    }
}

/// Extracted from `DatabaseHeader::usable_page_size` so its
/// `requires`/`ensures` can close statically instead of falling to
/// `layer: "runtime"` — the one genuine compile-time proof this whole
/// spike produced. Findings.md's round 3 found this needed explicit
/// `>= 0` bounds against v0.5.1; round 4 confirms mvl-lang/mvl-rust#94
/// (shipped v0.7.0) made those redundant — this is the v0.7.0-simplified
/// form. Expected: `compute_usable_page_size::returns::ensures#0` at
/// `layer: "L4"`, `warrant: "proof"`.
#[allow(clippy::arithmetic_side_effects)]
#[mvl::requires(reserved_space <= page_size)]
#[mvl::ensures(result <= page_size)]
pub fn compute_usable_page_size(page_size: u32, reserved_space: u32) -> u32 {
    page_size - reserved_space
}

// ---------------------------------------------------------------------
// Part 2: isolated, minimal repros for each native-solver gap this spike
// found — each independently confirmed by toggling exactly one variable
// and rerunning `cargo mvl prove`, not inferred from Part 1's combined
// annotations. See `findings.md` for the reasoning behind each one.
// ---------------------------------------------------------------------

/// mvl-lang/mvl-rust#94, fixed in v0.7.0: a `u32` parameter's implicit
/// `>= 0` used to need restating by hand (`reserved_space >= 0 &&
/// page_size >= 0`) before this could close; now it closes with just the
/// one real fact stated in `requires`. This is the same shape as
/// `compute_usable_page_size` above with the bound removed, kept
/// separate so it's independently attributable to #94 rather than mixed
/// in with Part 1's narrative. Expected:
/// `implicit_unsigned_bound_now_injected::returns::ensures#0` at
/// `layer: "L4"`.
#[allow(clippy::arithmetic_side_effects)]
#[mvl::requires(reserved_space <= page_size)]
#[mvl::ensures(result <= page_size)]
pub fn implicit_unsigned_bound_now_injected(page_size: u32, reserved_space: u32) -> u32 {
    page_size - reserved_space
}

/// mvl-lang/mvl-rust#95, fixed in v0.7.0: `self.field` now binds as its
/// own solver variable (it used to be opaque, pinning every `impl` method
/// to `runtime` regardless of how good the hypothesis was). Still needs
/// the `>= 0` bounds restated explicitly — #94's automatic injection
/// doesn't extend to field projections, only bare parameters (confirmed
/// by removing them here and watching it fall back to `runtime`).
/// Expected: `Page::usable_page_size_field_projection::returns::ensures#0`
/// at `layer: "L4"`.
pub struct Page {
    pub page_size: u32,
    pub reserved_space: u32,
}

impl Page {
    #[allow(
        clippy::arithmetic_side_effects,
        clippy::absurd_extreme_comparisons,
        unused_comparisons
    )]
    #[mvl::requires(
        self.reserved_space <= self.page_size
            && self.reserved_space >= 0
            && self.page_size >= 0
    )]
    #[mvl::ensures(result <= self.page_size)]
    pub fn usable_page_size_field_projection(&self) -> u32 {
        self.page_size - self.reserved_space
    }
}

/// Residual gap, only PARTIALLY fixed by mvl-lang/mvl-rust#113 (v0.7.1):
/// a cast on a **field projection** still blocks solver variable-binding,
/// even though the identical cast on a **bare parameter** is now fixed
/// (see `cast_on_bare_param_also_blocked` below, `layer: "L4"` as of
/// v0.7.1). This is exactly why `DatabaseHeader::usable_page_size` above
/// still can't be annotated directly and has to delegate to a free
/// function taking a pre-converted `u32`. Expected:
/// `PageWithNarrowField::usable_page_size_with_cast::returns::ensures#0`
/// at `layer: "runtime"`, unchanged despite identical, sufficient bounds.
pub struct PageWithNarrowField {
    pub page_size: u32,
    pub reserved_space: u8,
}

impl PageWithNarrowField {
    #[allow(
        clippy::arithmetic_side_effects,
        clippy::absurd_extreme_comparisons,
        unused_comparisons
    )]
    #[mvl::requires(
        self.reserved_space as u32 <= self.page_size
            && self.reserved_space as u32 >= 0
            && self.page_size >= 0
    )]
    #[mvl::ensures(result <= self.page_size)]
    pub fn usable_page_size_with_cast(&self) -> u32 {
        self.page_size - self.reserved_space as u32
    }
}

/// Same cast shape, isolated on a bare parameter instead of a field
/// projection. Fixed by mvl-lang/mvl-rust#113 (v0.7.1, round 5): this
/// now closes at **L4** — but `usable_page_size_with_cast` above (the
/// field-projection form) still doesn't, confirming #113 fixed casts on
/// bare parameters specifically, not casts in general. Expected:
/// `cast_on_bare_param_also_blocked::returns::ensures#0` at
/// `layer: "L4"`.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::absurd_extreme_comparisons,
    unused_comparisons
)]
#[mvl::requires(
    reserved_space as u32 <= page_size && reserved_space as u32 >= 0 && page_size >= 0
)]
#[mvl::ensures(result <= page_size)]
pub fn cast_on_bare_param_also_blocked(page_size: u32, reserved_space: u8) -> u32 {
    page_size - reserved_space as u32
}

#[derive(Debug)]
pub struct KnownShapeError;

/// mvl-lang/mvl-rust#97, fixed in v0.7.0: `Err(x).is_err()` on a
/// syntactically known `Result` construction now constant-folds to a
/// literal `bool` at L1, regardless of what `x` is. Expected:
/// `known_shape_fold_in_isolation::returns::ensures#0` (both return
/// sites) at `layer: "L1"` or `"L2"`.
#[mvl::ensures(result.is_err())]
pub fn known_shape_fold_in_isolation(x: i32) -> Result<i32, KnownShapeError> {
    if x < 0 {
        return Err(KnownShapeError);
    }
    Err(KnownShapeError)
}

/// The `||`-propagation half of this gap is fixed by
/// mvl-lang/mvl-rust#114 (v0.7.1, round 5): the early-`Err`-return
/// obligation (`ensures#0`) now closes at **L2** — `result.is_err()`
/// folds to `true` per #97, and #114 lets that `true` short-circuit the
/// whole `||` instead of stopping at the leaf. This is exactly why
/// `DatabaseHeader::parse`'s early-return obligations above now close
/// too.
///
/// The `Ok(x)`-return obligation (`ensures#1`) is a DIFFERENT, still-open
/// gap, not fixed by #113/#114/#115: closing it needs
/// `*result.as_ref().unwrap()` to resolve to the wrapped value `x` for
/// further reasoning against the branch-narrowed fact `x >= 0` — #97
/// only folds the outer `is_ok`/`is_err`/etc. call, it doesn't unwrap a
/// known-shape `Ok(x)`/`Some(x)` to `x` itself. This is the same root
/// limitation blocking `DatabaseHeader::parse`'s `Ok(...)` obligation
/// (`ensures#3`). NOT addressed by mvl-lang/mvl-rust#110 — see
/// `adr0011_licence_scope_does_not_reach_return_site_closure` below for
/// why that ticket, despite looking like the natural next step, turned
/// out to be scoped to a different code path entirely. Expected:
/// `ensures#0` at `layer: "L2"`, `ensures#1` still `layer: "runtime"`.
#[allow(clippy::unwrap_used)]
#[mvl::ensures(result.is_err() || *result.as_ref().unwrap() >= 0)]
pub fn known_shape_fold_not_propagated_through_or(x: i32) -> Result<i32, KnownShapeError> {
    if x < 0 {
        return Err(KnownShapeError);
    }
    Ok(x)
}

// ---------------------------------------------------------------------
// Part 3: mvl-lang/mvl-rust#110 (round 6, v0.8.0) — confirms empirically
// why this ticket, despite looking like the natural next step for
// `DatabaseHeader::parse`, doesn't move that obligation at all.
// ---------------------------------------------------------------------

/// The tool's own documented/tested shape for the licence (mirrors
/// `mvl-rust`'s `a_resolved_pure_helper_licenses_reflexivity_over_two_
/// identical_calls`): a same-file, explicitly `#[mvl::effect()]`,
/// zero-unresolved-call, non-float-returning function called twice with
/// identical arguments at ONE call site rewrites both calls to the same
/// opaque symbol, so `lo <= hi` becomes `x <= x` — proven by reflexivity.
/// Confirmed working exactly as documented. Expected:
/// `adr0011_licensed_reflexivity_over_two_identical_calls::calls::span_for_licence_demo::requires#0`
/// at `layer: "L1"`.
#[mvl::effect()]
pub fn gen_for_licence_demo() -> i64 {
    42
}

#[mvl::requires(lo <= hi)]
pub fn span_for_licence_demo(lo: i64, hi: i64) -> i64 {
    hi - lo
}

pub fn adr0011_licensed_reflexivity_over_two_identical_calls() -> i64 {
    span_for_licence_demo(gen_for_licence_demo(), gen_for_licence_demo())
}

/// Confirms the licence does NOT reach a function's own return-site
/// closure — the code path `DatabaseHeader::parse`'s postcondition
/// actually needs. `is_valid_page_size` is same-file and carries
/// `#[mvl::effect()]`, but its body is a method call
/// (`.is_power_of_two()`), which the tool's own test suite documents as
/// denying the licence outright (`an_effect_pure_function_with_a_method_
/// call_is_not_licensed` — a method-call body always counts as an
/// "unresolved call"). Confirmed: `validate`'s `ensures` stays `runtime`
/// even in the single most favorable case possible — the *exact same*
/// call, `is_valid_page_size(page_size)`, in both `requires` and
/// `ensures` of the same function, which would need `return_site_closure`
/// to reuse `requires` as an established Γ fact, not the
/// `obligations_for_call`/`propagate_postcondition` call-site machinery
/// #110 actually touches (per mvl-lang/mvl-rust#118's own PR
/// description). Expected: `validate::ensures#0` (declaration) and
/// `validate::returns::ensures#0` (return-site) both at `layer:
/// "runtime"`, unchanged by v0.8.0.
#[mvl::effect()]
pub fn is_valid_page_size(page_size: u32) -> bool {
    page_size.is_power_of_two() && (512..=65536).contains(&page_size)
}

#[mvl::requires(is_valid_page_size(page_size))]
#[mvl::ensures(is_valid_page_size(page_size))]
pub fn validate(page_size: u32) -> u32 {
    page_size
}
