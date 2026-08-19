# 0017 — A writable `Vm` shares one `Pager` via `RefCell<Pager>: PageSource`

**Status:** Accepted · **Date:** 2026-08-19

## Context

#194 (V3 phase 3, epic #161) needs the VDBE to run `Insert`/`Delete`/
`IdxInsert`/`NewRowid` against a real, write-capable `Pager` (`&mut
Pager`), while every existing read cursor (`OpenRead`, `Rewind`/`Next`/
`SeekRowid`/`Column`/`Rowid`) already depends on `VmDb::source: Rc<dyn
PageSource>` (ADR-0013) — an erased, `&self`-only, freely-`Rc`-clonable
read handle shared across N open cursors. A write-capable `Vm` needs
both: the existing shared-read shape for ordinary cursors, and exclusive
mutable access to the same underlying `Pager` for the write opcodes.

Three shapes were considered for bridging the two:

1. Make `TableCursor`/`CursorSlot::Table` generic over an access mode
   (read vs. write), duplicating traversal logic or introducing an enum
   inside `TableCursorState` for "how do I reach the pager".
2. Store two independent `Pager`s (or re-open the file a second time) —
   one for reads, one for writes.
3. Wrap one `Pager` in `Rc<RefCell<Pager>>`, implement `PageSource` for
   `RefCell<Pager>` (delegating to `Pager::read_page` through
   `self.borrow()`), and hold two `Rc` clones on `VmDb`: one unsized to
   `Rc<dyn PageSource>` for the pre-existing read path, one kept concrete
   for write opcodes to `.borrow_mut()`.

Option 1 would touch every read-cursor opcode to thread a second access
mode through, for a benefit (shared traversal code) that write opcodes
don't actually need — `Insert`/`Delete`/`NewRowid` never traverse via
`TableCursor`'s own stack-based iterator; `Delete` reads a rowid `Rewind`/
`Next` already computed, `Insert` doesn't traverse at all, and
`NewRowid`'s only traversal (`TableCursor::last()`) is a fresh, one-shot,
read-only cursor build. Option 2 would let a write through one `Pager`
become invisible to reads through the other until an explicit flush —
exactly the "unflushed write invisible to a same-connection read"
correctness bug spec 007's `Pager` was built to prevent (`dirty` map
consulted ahead of `wal_pages`/`source`).

## Decision

Implement `PageSource for RefCell<Pager>` (`src/pager.rs`) and give
`VmDb` a `writer: Option<Rc<RefCell<Pager>>>` field alongside its
existing `source: Rc<dyn PageSource>` (`src/vdbe/exec.rs`).
`Vm::with_writable_db(pager, header)` wraps `pager` once
(`Rc::new(RefCell::new(pager))`), unsizes one clone into `source` (so
every existing read-cursor code path — `CursorSlot::Table`,
`TableCursor<Rc<dyn PageSource>>` — works completely unchanged against a
writable `Vm`, satisfying spec 010 Requirement 1), and keeps the other
clone concrete in `writer` so write opcodes can `pager.borrow_mut()` it.
`Vm::with_db` (read-only) leaves `writer: None`; every write opcode
checks it first and returns `ExecError::NoDatabase` when absent.

A successful `Halt` (code 0) against a writer-bearing `Vm` calls
`Pager::flush()` before returning (`src/vdbe/exec.rs::run`) — this VM has
no explicit `COMMIT` opcode yet (`Transaction` remains a no-op), so a
clean halt is the closest available "this statement's writes are done"
signal, and a write invisible to a subsequently-`Pager::open`ed reader
would fail spec 010 Requirement 1's round-trip scenario outright.

## Alternatives rejected

- Generic/dual-mode `TableCursor`/`CursorSlot::Table` (option 1 above) —
  real mechanical cost for a duplication write opcodes don't need; see
  Context.
- Two independent `Pager`s over the same file (option 2) — reopens the
  exact unflushed-write-invisibility bug class spec 007's single-`Pager`
  `dirty`-map-first read ordering exists to close.
- A dedicated non-`Rc` wrapper struct implementing `PageSource` by
  borrowing a `&Pager` with an explicit lifetime, instead of `RefCell` —
  would need `Vm`/`VmDb` to carry a lifetime parameter, a larger, more
  invasive change than this ticket's scope for no behavioral difference
  (both approaches serialize page access to one underlying `Pager`).
- Flushing eagerly after every individual write opcode instead of once on
  `Halt` — needless I/O per row for a bulk `INSERT ... SELECT`-shaped
  program; nothing in this ticket's scope needs mid-statement durability
  finer than "flushed by the time the statement's `Halt` returns".

## Consequences

`RefCell<Pager>`'s runtime borrow-checking (`borrow_mut()` panicking on
a conflicting outstanding borrow) is a new failure mode absent from the
read-only path, but is unreachable under this ticket's dispatch
discipline: no opcode handler holds a `Ref`/`RefMut` across a call into
another opcode handler, and `Vm`'s single-threaded, one-instruction-at-a-
time execution loop never re-enters `dispatch` while a borrow from an
earlier instruction is still live. `Vm::with_writable_db` becomes the
second constructor (`ADR-0013`'s `Rc<dyn PageSource>` boundary already
established) — any future opcode needing exclusive `Pager` access reuses
`VmDb::writer` rather than inventing a third plumbing shape. A real
`COMMIT`/transaction-boundary opcode (out of this ticket's scope) will
need to revisit the "flush on every successful `Halt`" simplification
once multi-statement transactions matter.
