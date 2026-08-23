# Spike 010: `rust-refine` proof-of-concept

Issue [#371](https://github.com/iheitlager/sqlite-rs/issues/371). Full
narrative, round-by-round evidence, and open gaps for future work are in
[`findings.md`](./findings.md) — this file is the short version.

## Hypothesis

Issue #371's own success criteria: annotate `DatabaseHeader::parse`'s
real invariants (`page_size` a power of two in `512..=65536`,
`reserved_space < page_size`, `buf.len() >= HEADER_LEN`) with `rust-refine`
(one of the five `mvl-lang/mvl-rust` tools), and see whether they
**discharge at L1–L3 or fall to a runtime residue** — either outcome is
useful, but the hypothesis worth testing was that a small, pure,
already-validated invariant set like this is close to the ideal case for
a refinement-type checker, so it should discharge cleanly if the tool
works as advertised on real (not toy-example) Rust.

That hypothesis did not survive contact with `impl`-heavy,
`Result`-returning code: every one of the tool's stated capabilities
turned out to have a real-code gap invisible in the tool's own toy
examples, and each was found, reported, and fixed **same-day** across
four rounds of testing.

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
see gap 3 below for why) with two parts: a corrected recreation of the
issue's actual target (`DatabaseHeader`/`parse`/`usable_page_size`), and
isolated one-function-per-gap repros. Every function's doc comment states
an `Expected:` `cargo mvl prove` layer, checked against real output
before being written down.

## Links

- Issue: [sqlite-rs#371](https://github.com/iheitlager/sqlite-rs/issues/371)
- Fixes shipped from this spike's findings, in order:
  - [mvl-lang/mvl-rust#90](https://github.com/mvl-lang/mvl-rust/pull/90) — `impl` methods invisible to the scanner
  - [mvl-lang/mvl-rust#92](https://github.com/mvl-lang/mvl-rust/issues/92) / [#93](https://github.com/mvl-lang/mvl-rust/pull/93) — E0282 on an `ensures` referencing an `Ok`-field at an early return
  - [mvl-lang/mvl-rust#94](https://github.com/mvl-lang/mvl-rust/issues/94) — no implicit unsigned lower bound
  - [mvl-lang/mvl-rust#95](https://github.com/mvl-lang/mvl-rust/issues/95) — `self.field` not bound as a solver variable
  - [mvl-lang/mvl-rust#97](https://github.com/mvl-lang/mvl-rust/issues/97) — known-shape `Result`/`Option` methods not constant-folded
- Full write-up: [`findings.md`](./findings.md), including 3 further open
  gaps (cast expressions, `||` short-circuit, `pub mod` scanning) not yet
  filed upstream.

## Conclusion

**Go, for runtime enforcement; still short of a real static proof on
this codebase's actual invariants.** Across four rounds against
successive `mvl-rust` releases (v0.4.0 → v0.7.0), five real gaps in the
tool were found and three were fixed same-day from this spike's reports
alone — a strong signal the project is responsive and the tool is
improving fast. But the net result on `header.rs`'s real invariants is
exactly one obligation (`compute_usable_page_size`'s postcondition, a
free function extracted specifically to work around the `self.field`/cast
gaps) that discharges at a genuine static layer (**L4**); everything else,
including `DatabaseHeader::parse`'s actual field-validation postcondition
— the thing issue #371 was written to test — still falls to `layer:
"runtime"` (equivalent to a hand-written `assert!`, not stronger).

Not adopted in the main `sqlite-rs` package (see `findings.md`'s
"Disposition") — this crate is the reference point for re-evaluating
once the three gaps listed there are addressed upstream.
