# Spike 010: does `rust-refine` work, and does it make sense for sqlite-rs?

**Status: concluded — yes.** Issue
[#371](https://github.com/iheitlager/sqlite-rs/issues/371). Full
narrative, round-by-round evidence, and tracked upstream issues are in
[`findings.md`](./findings.md) — this file is the short version.
Production rollout is now its own ticket (see "Next steps"); this crate
stays in place as the reference point, not deleted.

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
same-day, sometimes twice in one day, across six rounds against
successive releases. As of round 5 (v0.7.1), 3 of `parse`'s 4
obligations discharge as genuine compile-time proofs. Round 6 (v0.8.0)
tested — rather than assumed — whether the newest upstream feature
would close the last one; it doesn't, and now we know precisely why.

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
see [#115](https://github.com/mvl-lang/mvl-rust/issues/115)) with three
parts: a corrected recreation of the issue's actual target
(`DatabaseHeader`/`parse`/`usable_page_size`), isolated
one-function-per-gap repros, and (round 6) a direct empirical test of
`mvl-lang/mvl-rust#110`'s actual scope. Every function's doc comment
states an `Expected:` `cargo mvl prove` layer, checked against real
output before being written down.

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
- **Tested and found NOT to apply (round 6):**
  [mvl-lang/mvl-rust#110](https://github.com/mvl-lang/mvl-rust/issues/110)/
  [#118](https://github.com/mvl-lang/mvl-rust/pull/118) (implementing
  [ADR-0011](https://github.com/mvl-lang/mvl-rust/blob/main/.openspec/adr/0011-resolved-pure-closure-licence.md),
  design in [#103](https://github.com/mvl-lang/mvl-rust/issues/103)),
  shipped in v0.8.0. Round 5 guessed this would be `parse`'s last
  unlock; round 6 tested it directly and found it's scoped to
  cross-function call-site obligations, not a function's own
  return-site closure — the code path `parse`'s postcondition actually
  needs. Confirmed empirically with two new repros in `src/lib.rs`, not
  inferred from the PR description alone.
- **Still open, not yet filed:**
  - Cast on a field projection (vs. a bare parameter) — narrower half of #113
  - Method-call/struct-field reasoning inside a function's own
    return-site closure — the actual remaining blocker for `parse`,
    now precisely named, previously conflated with #110
  - [mvl-lang/mvl-rust#115](https://github.com/mvl-lang/mvl-rust/issues/115) — `pub mod` scanning, not independently re-verified since being filed
- Full write-up: [`findings.md`](./findings.md)

## Conclusion

**Yes — and this round made the "not yet" part smaller and more honest,
not bigger.**

- **Does the tool work?** Yes. Five real defects found across six
  rounds, every one fixed by the maintainer same-day, from reports filed
  directly from this spike. Round 6 adds a data point in the other
  direction, and it's a healthy one: not every upstream change closes
  this spike's gap, and this spike caught that immediately by testing
  rather than assuming — which is exactly the discipline that made the
  first five rounds trustworthy.
- **Does it make sense to adopt?** Yes, unconditionally, for runtime
  enforcement: `#[mvl::requires]`/`#[mvl::ensures]` on real `impl`
  methods compile, are scanned, and are enforced with a real `assert!`
  at every return path, including every early-return branch.
- **Does it prove things at compile time, for invariants like this
  codebase's?** Mostly. `DatabaseHeader::parse`'s early-return
  obligations — 3 of its 4 return sites — discharge at **L1**. The 4th
  (the field-validation success case) is still `runtime`, and after
  round 6, the reason is precisely named rather than hoped-away: the
  native solver has no path for a function to reason about its own
  unwrapped `Ok(x)`'s fields or method calls on them within its own
  return-site closure. Cross-function call obligations (what #110
  actually fixed) are a different, already-working code path.

**Verdict: yes, `rust-refine` works and makes sense for `sqlite-rs`,**
for runtime-enforced contracts today, unconditionally, and for full
static proof of `header.rs`-shaped invariants once the *actual*
remaining gap — same-function return-site method-call reasoning, not
#110 — gets its own tracked fix. That gap isn't filed yet; this spike's
job now is to file it precisely, the same way #94/#95/#97/#113/#114
were, rather than keep guessing at which upstream ticket might
incidentally cover it.

## Next steps

1. **File the actual remaining gap**, now precisely scoped by round 6:
   a function's own `return_site_closure` needs a way to reason about
   an unwrapped known-shape value's fields/method-call results, distinct
   from #110's cross-function call-site licence. Use `validate` and
   `DatabaseHeader::parse` in `src/lib.rs` as the repro, the same way
   prior rounds' issues were filed.
2. **File the narrower cast-on-field-projection follow-up** to #113 —
   still open, not yet its own issue.
3. **Once the return-site-closure gap is fixed and `parse`'s
   postcondition is fully proven:** propose adopting
   `#[mvl::requires]`/`#[mvl::ensures]` on real `src/header.rs` (or
   wider) in the main `sqlite-rs` package as a proper feature ticket
   with its own token-spend estimate — not as a spike. At that point the
   answer to "does it make sense" moves from "yes, in principle, proven
   on a recreation" to "yes, proven on the actual production code."
4. **Independent of 1-3:** the runtime-enforcement value is available
   now and doesn't need any of the above. If runtime-checked contracts
   on `impl` methods are wanted sooner, that's a separate, smaller
   proposal than "wait for full static proof."
