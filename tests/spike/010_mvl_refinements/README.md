# Spike 010: does `rust-refine` work, and does it make sense for sqlite-rs?

Issue [#371](https://github.com/iheitlager/sqlite-rs/issues/371). Full
narrative, round-by-round evidence, and tracked upstream issues are in
[`findings.md`](./findings.md) — this file is the short version.

## Hypothesis

This is an experiment in two things at once, not one: **does the tool
work**, and **does adopting it make sense for this codebase**. The
expectation going in was yes to both — `rust-refine` annotating
`DatabaseHeader::parse`'s real, already-hand-validated invariants
(`page_size` a power of two in `512..=65536`, `reserved_space <
page_size`, `buf.len() >= HEADER_LEN`) looked like close to the ideal
case for a refinement-type checker: small, pure, no I/O, invariants
already known and tested.

It didn't work on the first try — not because the idea was wrong, but
because the tool, as shipped, had never been run against real
`impl`-heavy, `Result`-returning Rust before. Every gap found was fixed
same-day, sometimes twice in one day, across five rounds against
successive releases. As of round 5 (v0.7.1), most of `parse`'s actual
obligations discharge as genuine compile-time proofs. The hypothesis
holds.

## Setup

Self-contained crate, no dependency on the rest of `sqlite-rs`:

```bash
make help    # list all targets
make build   # cargo build
make test    # runtime_enforcement.rs — confirms the assert! injections fire correctly
make lint    # fmt --check + clippy --all-targets
make prove   # cargo mvl prove src/lib.rs — requires cargo-mvl installed at this
             # crate's pinned mvl rev (see Cargo.toml), e.g.:
             #   cargo install --git https://github.com/mvl-lang/mvl-rust \
             #     --rev <rev from Cargo.toml> cargo-mvl --locked
```

`src/lib.rs` is deliberately a flat file (no `pub mod { ... }` wrappers —
see [#115](https://github.com/mvl-lang/mvl-rust/issues/115)) with two
parts: a corrected recreation of the issue's actual target
(`DatabaseHeader`/`parse`/`usable_page_size`), and isolated
one-function-per-gap repros. Every function's doc comment states an
`Expected:` `cargo mvl prove` layer, checked against real output before
being written down.

## Links

- Issue: [sqlite-rs#371](https://github.com/iheitlager/sqlite-rs/issues/371)
- Fixed, from this spike, in order:
  - [mvl-lang/mvl-rust#90](https://github.com/mvl-lang/mvl-rust/pull/90) — `impl` methods invisible to the scanner
  - [mvl-lang/mvl-rust#92](https://github.com/mvl-lang/mvl-rust/issues/92) / [#93](https://github.com/mvl-lang/mvl-rust/pull/93) — E0282 on an `ensures` referencing an `Ok`-field at an early return
  - [mvl-lang/mvl-rust#94](https://github.com/mvl-lang/mvl-rust/issues/94) — no implicit unsigned lower bound
  - [mvl-lang/mvl-rust#95](https://github.com/mvl-lang/mvl-rust/issues/95) — `self.field` not bound as a solver variable
  - [mvl-lang/mvl-rust#97](https://github.com/mvl-lang/mvl-rust/issues/97) — known-shape `Result`/`Option` methods not constant-folded
  - [mvl-lang/mvl-rust#113](https://github.com/mvl-lang/mvl-rust/issues/113) — see through `as` casts on bare parameters (partial — field-projection casts still open, see below)
  - [mvl-lang/mvl-rust#114](https://github.com/mvl-lang/mvl-rust/issues/114) — propagate boolean short-circuit after L1 method-call folding — **the fix that moved `parse`'s obligations**
  - [mvl-lang/mvl-rust#116](https://github.com/mvl-lang/mvl-rust/pull/116) — the PR that closed #113/#114/#115 together, v0.7.1
- **Still open, narrower than when filed:**
  - Cast on a field projection (vs. a bare parameter) — not yet its own follow-up issue
  - [mvl-lang/mvl-rust#115](https://github.com/mvl-lang/mvl-rust/issues/115) — `pub mod` scanning, not independently re-verified this round
- **The bigger unlock, in progress upstream, not from this spike:**
  [mvl-lang/mvl-rust#110](https://github.com/mvl-lang/mvl-rust/issues/110)
  (implementing [ADR-0011](https://github.com/mvl-lang/mvl-rust/blob/main/.openspec/adr/0011-resolved-pure-closure-licence.md),
  design in [#103](https://github.com/mvl-lang/mvl-rust/issues/103)) — a
  sound purity licence that would let a syntactically known `Ok(x)`/
  `Some(x)` unwrap to `x` for further reasoning. This is the one thing
  left blocking `parse`'s final obligation (the field-validation success
  case).
- Full write-up: [`findings.md`](./findings.md)

## Conclusion

**Yes.**

- **Does the tool work?** Yes. Every defect found — five of them, across
  five rounds — was fixed by the maintainer, same-day every time, from
  reports filed directly from this spike. That's not a hypothetical
  "the tool could work"; it's a demonstrated, fast, responsive
  development loop against real feedback.
- **Does it make sense to adopt?** Yes. `#[mvl::requires]`/
  `#[mvl::ensures]` on real `impl` methods compile, are scanned, and are
  enforced with a real `assert!` at every return path, including every
  early-return branch. That alone is a working contract-enforcement
  layer, adoptable today.
- **Does it prove things at compile time, for invariants like this
  codebase's?** As of v0.7.1: mostly yes. `DatabaseHeader::parse`'s
  early-return obligations — 3 of its 4 return sites — now discharge at
  **L1**, with zero changes to the annotation between round 4 and round
  5. Only the actual field-validation success case
  (`page_size.is_power_of_two()` etc., after unwrapping `Ok(...)`)
  remains `runtime`, and that gap has a name and a tracked, in-progress
  fix (#110).

The honest caveat: this spike's own target invariant — the specific
thing issue #371 asked to prove — is not *fully* proven yet. One
obligation out of `parse`'s four is still a runtime assertion, not a
static proof. But the trajectory across five rounds is unambiguous, and
nothing about the remaining gap looks structurally hard — it's a named,
scoped, in-progress feature (#110), not an open research question.

**Verdict: yes, `rust-refine` works and makes sense for `sqlite-rs`,**
for runtime-enforced contracts now, and for full static proof of
`header.rs`-shaped invariants once #110 lands — which, given this
project's track record, is a matter of when, not if.

## Next steps

1. **Watch #110/ADR-0011.** This is the only thing left between here and
   a fully-proven `DatabaseHeader::parse`. Once it lands, rerun
   `make prove` in this crate — `DatabaseHeader::parse`'s `ensures#3`
   (the `Ok(...)` obligation) and `usable_page_size_with_cast` are the
   two functions to check first.
2. **File the narrower cast-on-field-projection follow-up** to #113 if
   it isn't already tracked upstream by the time #110 work starts —
   worth confirming rather than assuming it'll be swept up incidentally.
3. **Once #110 lands and `parse`'s postcondition is fully proven:**
   propose adopting `#[mvl::requires]`/`#[mvl::ensures]` on real
   `src/header.rs` (or wider) in the main `sqlite-rs` package as a
   proper feature ticket with its own token-spend estimate — not as a
   spike. At that point the answer to "does it make sense" moves from
   "yes, in principle, proven on a recreation" to "yes, proven on the
   actual production code."
4. **Independent of 1-3:** the runtime-enforcement value is available
   now and doesn't need any of the above. If runtime-checked contracts
   on `impl` methods are wanted sooner, that's a separate, smaller
   proposal than "wait for full static proof."
