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
same-day, sometimes twice in one day, across four rounds against
successive releases. That responsiveness is itself part of the answer.

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
- **Open, filed from this spike, not yet fixed:**
  - [mvl-lang/mvl-rust#113](https://github.com/mvl-lang/mvl-rust/issues/113) — see through `as` casts when extracting solver variables
  - [mvl-lang/mvl-rust#114](https://github.com/mvl-lang/mvl-rust/issues/114) — propagate boolean short-circuit after L1 method-call folding (#97 follow-up)
  - [mvl-lang/mvl-rust#115](https://github.com/mvl-lang/mvl-rust/issues/115) — obligation scanning invisible inside `pub mod { ... }` blocks
- **The bigger unlock, in progress upstream, not from this spike:**
  [mvl-lang/mvl-rust#110](https://github.com/mvl-lang/mvl-rust/issues/110)
  (implementing [ADR-0011](https://github.com/mvl-lang/mvl-rust/blob/main/.openspec/adr/0011-resolved-pure-closure-licence.md),
  design in [#103](https://github.com/mvl-lang/mvl-rust/issues/103)) — a
  sound purity licence that would let same-file method calls like
  `is_power_of_two()` participate in a proof instead of being opaque to
  the solver. If this lands, it's the thing that would move `parse`'s
  actual postcondition off `runtime` for the first time.
- Full write-up: [`findings.md`](./findings.md)

## Conclusion

**Yes — with a precise scope on what "yes" means today.**

- **Does the tool work?** Yes. Every blocking defect found (impl-method
  invisibility, a type-inference compile error, silent zero-obligation
  passes) is fixed, and the fix turnaround was same-day, three times, on
  reports from this spike alone. That's a strong, concrete signal this
  is a maintained, responsive tool, not an abandoned experiment.
- **Does it make sense to adopt?** Yes, for what it reliably delivers
  today: `#[mvl::requires]`/`#[mvl::ensures]` on real `impl` methods
  compile, are picked up by the scanner, and are enforced with a real
  `assert!` at every return path — including every early-return branch,
  which upstream's own tail-only instrumentation used to miss and this
  fork improved on. That's a genuine, working contract-enforcement layer
  on top of ordinary Rust, adoptable now, independent of anything below.
- **Does it prove things at compile time yet, for invariants like this
  codebase's?** Not fully. One obligation in this whole spike
  (`compute_usable_page_size`'s postcondition — pure integer arithmetic,
  no casts, no method calls) discharges at a real static layer (L4).
  `DatabaseHeader::parse`'s actual postcondition — the one issue #371
  was written to test — still falls to `runtime`, blocked by casts
  (#113) and method-call reasoning (#110/#114) that aren't in the native
  solver's fragment yet.

So: **the tool works, and adopting it for runtime enforcement makes
sense now.** The compile-time-proof case for *this specific codebase's*
invariants isn't proven yet — it's pending #113 (small, targeted) and
#110 (the real unlock, larger). Not a "no"; a "not yet, and here's
exactly what closes it."

## Next steps

1. **Watch #113/#114/#115** — small, targeted, and this project has
   closed everything else from this spike same-day. Re-run
   `make prove` here once any land; update `findings.md`.
2. **Watch #110/ADR-0011** — this is the one that actually matters for
   `header.rs`-shaped code: if same-file method calls become
   provable, re-annotate `DatabaseHeader::parse` with its full,
   real postcondition (already written, in `src/lib.rs`) and check
   whether it closes above `runtime` for the first time.
3. **If/when #110 lands:** re-run this spike, update the go/no-go, and
   *then* decide whether to propose adopting `#[mvl::requires]`/
   `#[mvl::ensures]` on real `src/header.rs` (or wider) in the main
   `sqlite-rs` package — as a proper feature ticket with its own
   token-spend estimate, not as a spike.
4. **Independent of 1-3:** the runtime-enforcement value is available
   now and doesn't need any of the above. If runtime-checked contracts
   on `impl` methods are wanted sooner, that's a separate, smaller
   proposal than "wait for static proof."
