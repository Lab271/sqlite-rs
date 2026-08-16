# Spike 009: eliminating `dyn` from the VFS trait-object boundary — findings

Issue #80. Branch `spike/009_vfs_dyn_elimination`. Disposed per spike
convention: the throwaway crate (`Cargo.toml`, `src/option_a.rs`,
`src/option_b.rs`) is deleted in the closing commit; this findings doc
survives.

## Scope

Prototyped both options against the `Vfs`/`VfsFile`/`PageSource`/
`VfsPageSource` slice (mirroring `src/vfs.rs`, `src/vfs/page_source.rs`,
`src/vfs/{unix,memory}.rs`) plus the one real generic-would-propagate-to
consumer, `Pager` (`src/pager.rs`). `SharedLockGuard`/`FileLock` are out
of scope, per the issue.

- Option A: `src/option_a.rs` (154 lines) — associated-type `Vfs`,
  generic `VfsPageSource<F: VfsFile>` and `Pager<V: Vfs>`.
- Option B: `src/option_b.rs` (139 lines) — closed `enum AnyVfs`/
  `AnyVfsFile`, `VfsPageSource`/`Pager` stay non-generic (bit-for-bit
  the same shape as today's `src/pager.rs`).

Both compile clean and both pass `cargo-mvl-limit` on the prototype
source files (no `dyn` anywhere) — confirmed by running the gate
directly against `option_a.rs`/`option_b.rs`.

## Option A — associated type

**What changes, concretely:**
- `Vfs` gains `type File: VfsFile;`; `open_read` returns `Self::File`.
- `VfsPageSource` becomes `VfsPageSource<F: VfsFile>` — a new type
  parameter that has to be spelled at every place that names the type:
  struct field, `impl` block, and the `open` associated function's own
  `<V: Vfs<File = F>>` bound.
- `Pager` becomes `Pager<V: Vfs>`, storing `VfsPageSource<V::File>`.
  Real-world equivalent: every one of `src/pager.rs`'s current
  `Pager` usages, plus the 3 test call sites in `src/btree.rs`,
  `src/btree/index.rs`, `src/schema/ddl_reader.rs` that build a
  `TableCursor<VfsPageSource>` / `IndexCursor<VfsPageSource>` today,
  would need `VfsPageSource<UnixFile>` (or `<F>` if the test itself
  stays generic) spelled explicitly — a mechanical but real rename at
  4+ sites outside `src/vfs/`.
- **Runtime backend selection still needs its own erasure.** The
  prototype's `AnyPager` enum (`Unix(Pager<UnixVfs>) | Memory(Pager<MemoryVfs>)`)
  demonstrates this directly: choosing Unix vs Memory from a CLI flag at
  runtime can't produce "a `Pager<V>`, don't care which `V`" without
  wrapping it in *something* — an enum (i.e. re-deriving Option B one
  layer up) or a `dyn` (i.e. not eliminating it, just moving it).
  sqlite-rs doesn't do this today — the read path currently always
  knows its backend statically at the call site — but any future CLI
  or config-driven backend choice would reintroduce this problem.

## Option B — closed enum dispatch

**What changes, concretely:**
- `AnyVfsFile`/`AnyVfs` each need one `match self { ... }` arm per
  trait method per variant. With 2 variants (Unix, Memory) and 2
  methods on `VfsFile` in the real code (`read_at`, `size`, plus
  `lock_shared` if in scope) and 3 on `Vfs` (`open_read`, `exists`,
  `claim_wal_read_lock`), that's `2 variants × 5 methods` = 10 match
  arms just for these two traits in the real codebase (prototype only
  exercises `read_at`/`size`/`open_read`/`exists` = 3 `match self`
  blocks, real surface is larger).
- `VfsPageSource` and `Pager` need **zero** changes — they stay exactly
  the concrete, non-generic types they are today on `main`. This is the
  headline result: the "generic has to thread one layer further" cost
  Option A pays doesn't exist here.
- Runtime backend selection is free — `AnyVfs::Memory` vs
  `AnyVfs::Unix` picked at runtime is just an enum value, no wrapper
  needed (demonstrated in `option_b.rs`'s `main`).
- **Boilerplate scales with (traits × methods × backends), confirmed.**
  The real codebase has 3 traits in the `dyn` boundary today (`Vfs`,
  `VfsFile`, `SharedLockGuard`), not the 2 this spike sliced. Adding
  `SharedLockGuard` (1 method, `unlock`-via-`Drop` only, no explicit
  trait method beyond the marker) is cheap, but every method added to
  `Vfs`/`VfsFile` in the future costs `2 × 1` new match arms instead of
  `1` new trait-method body under Option A.

## Line-count comparison (this slice only)

| | Option A | Option B |
|---|---|---|
| Total prototype lines | 154 | 139 |
| `match self` dispatch blocks | 1 (`AnyPager`, opt-in demo only) | 3 (required: `AnyVfsFile::read_at`, `AnyVfsFile::size`, `AnyVfs::open_read`) |
| New type parameters introduced | 2 (`VfsPageSource<F>`, `Pager<V>`) | 0 |
| Call sites outside `src/vfs/` that change | 4+ (`Pager`, `src/btree.rs`, `src/btree/index.rs`, `src/schema/ddl_reader.rs`) | 0 |

Option B is smaller in this slice specifically *because* the real
codebase's `Pager`/`TableCursor`/`IndexCursor` never need to be generic
over the backend — they're built once, from one concrete `Vfs` impl,
per process. Option A's cost (generic propagation) is paid precisely at
the boundary sqlite-rs doesn't currently use (multi-backend-at-runtime),
while Option B's cost (match-arm boilerplate) is paid at a boundary
that's small and fixed (2 backends, ~5 methods total).

## Recommendation

**Keep the `dyn` exclusion as-is. Neither option is worth it today.**

Rationale:
- Option A's real cost — generic parameters threading through `Pager`
  and every test call site — buys static dispatch and no allocation
  for a code path (VFS file open) that happens once per database
  connection, not in a hot loop. There's no measured or plausible
  perf motivation here; `Box<dyn VfsFile>` is one allocation at
  connection-open time.
- Option B's real cost — match-arm boilerplate scaling with
  `traits × methods × backends` — buys nothing sqlite-rs doesn't
  already have via `dyn`: the backend set is closed at 2 variants
  either way, so there's no plugin/extensibility win to justify the
  boilerplate over the existing one-line-per-method `dyn` calls.
- The `mvl-limit` exclusion for these 4 files is deliberate and
  documented (`src/vfs/page_source.rs`'s own doc comment, and the
  Makefile target's comment) precisely because this boundary is
  narrow (4 files) and stable (no new backends have been added since
  `UnixVfs`/`MemoryVfs`). Spending a rewrite here doesn't reduce risk,
  doesn't improve performance measurably, and doesn't unlock a
  currently-blocked feature — it only removes 4 lines from a
  Makefile exclusion list.
- If sqlite-rs later adds a third VFS backend, or a runtime-selectable
  backend (the CLI-flag scenario noted under Option A), that's the
  point to re-run this comparison — the calculus changes once the
  backend count actually grows, or once "don't care which Vfs at
  runtime" becomes a real requirement instead of a hypothetical.

No follow-up implementation ticket is warranted from this spike.
