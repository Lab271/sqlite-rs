//! Spike 010: `rust-refine` (mvl-lang/mvl-rust) proof-of-concept, standalone.
//!
//! Issue #371. See `findings.md` in this directory for the full narrative
//! across four rounds of testing against successive `mvl-rust` releases.
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
    /// v0.5.1+ (mvl-lang/mvl-rust#90/#93) — findings.md's rounds 1-2. All
    /// 8 obligations (1 per return site) still discharge at
    /// `layer: "runtime"` as of v0.7.0 — findings.md's round 4, "#97
    /// doesn't reach `parse`".
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
    /// necessary even after mvl-lang/mvl-rust#95.
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

/// Residual gap, NOT fixed by #95: an `as` cast blocks solver
/// variable-binding regardless of whether the operand is a field
/// projection (as here) or a bare parameter (confirmed both ways — see
/// `cast_on_bare_param_also_blocked` below). This is exactly why
/// `DatabaseHeader::usable_page_size` above still can't be annotated
/// directly and has to delegate to a free function taking a
/// pre-converted `u32`. Expected:
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

/// Same cast-blocks-binding gap, isolated on a bare parameter instead of
/// a field projection — confirms the gap is about the cast expression
/// itself, not specifically about `self.field`. Expected:
/// `cast_on_bare_param_also_blocked::returns::ensures#0` at
/// `layer: "runtime"`.
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

/// Residual gap, NOT fixed by #97: the same fold does not propagate
/// through `||` to short-circuit a combined expression to `true` when the
/// second clause still contains something out of #97's scope (`.unwrap()`
/// here). This is exactly why `DatabaseHeader::parse`'s `ensures` above
/// stays at `runtime` for every early-return obligation despite #97.
/// Expected: `known_shape_fold_not_propagated_through_or::returns::ensures#0`
/// for the early-`Err`-return site at `layer: "runtime"`, even though
/// `result.is_err()` alone (see above) would fold to `true` on its own.
#[allow(clippy::unwrap_used)]
#[mvl::ensures(result.is_err() || *result.as_ref().unwrap() > 0)]
pub fn known_shape_fold_not_propagated_through_or(x: i32) -> Result<i32, KnownShapeError> {
    if x < 0 {
        return Err(KnownShapeError);
    }
    Ok(x)
}
