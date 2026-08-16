# 0013 — VDBE keeps a second `dyn` boundary for `PageSource` (rejects generic `Vm`)

**Status:** Accepted · **Date:** 2026-08-16

## Context

`src/vdbe/exec.rs` and `src/vdbe/cursor.rs` hold `Rc<dyn PageSource>` and were
exempted from the `mvl-limit` qualified-subset gate (#90) while the gate was
being restored to CI (follow-up to #70). #114 proposed closing that exemption
by making `Vm` generic over `P: PageSource`, matching the pattern already
used by `Pager::open::<V: Vfs>` (spec 007) and `TableCursor<P>` (spec 006) —
the reference shape that keeps every `dyn` boundary inside `src/vfs/`.

`Vm` has no database in the common case (`db: Option<VmDb>`); every
arithmetic/control/sorter program uses the no-database `Vm::new()`. A
generic `Vm<P>` would force that constructor to name a concrete `P`, break
`Vm`'s `#[derive(Default)]` (a page source is not `Default`), and thread
`<P: PageSource>` through every opcode handler — roughly 94 references
across 7 files. The `Rc` erasure exists because a `Vm` shares one page
source across N open cursors via cheap clones so they never contend over
exclusive ownership of the file handle; that sharing need doesn't go away
under a generic `Vm`, so real callers would still instantiate
`Vm<Rc<dyn PageSource>>` — the same runtime dispatch, just with the `dyn`
moved one type parameter over instead of removed.

## Decision

Accept `Rc<dyn PageSource>` in `src/vdbe/{exec,cursor}.rs` as a second,
permanent, narrow `dyn` boundary alongside `src/vfs/`. Close #114 without
implementing it. `MVL_LIMIT_EXCLUDE` keeps `src/vdbe/exec.rs` and
`src/vdbe/cursor.rs` alongside the VFS files and `src/bin/*`.

## Alternatives rejected

- Generic `Vm<P: PageSource>` (#114 as filed) — mechanical churn across ~94
  call sites and 7 files, a broken `Default` derive, and a real chance of
  landing on `Vm<Rc<dyn PageSource>>` anyway, which moves the boundary
  rather than removing it.
- A no-op/uninhabited `PageSource` impl or a default type parameter to
  paper over the no-database case — adds a fictitious type solely to satisfy
  the type checker, no clearer than the status quo.
- Splitting `Vm` into a database-less and a database-holding variant —
  a larger structural change than the ticket's stated scope, and not
  justified by a problem this ADR needs to solve.

## Consequences

`mvl-limit`'s qualified subset is `src/vfs/` + `src/vdbe/{exec,cursor}.rs`
+ `src/bin/*`, not VFS-only — a materially weaker claim than #114's original
target, stated as permanent policy rather than an open exception. Specs
006-btree and 007-pager, and the Makefile's boundary-policy comment, cite
this ADR instead of treating #114 as pending. A future change that wants a
stronger claim needs a superseding ADR, not a quiet reopen of #114.
