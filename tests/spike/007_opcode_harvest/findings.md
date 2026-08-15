# Spike 007 — opcode harvest via oracle EXPLAIN (#58)

Pure tooling — `tools/harvest_opcodes.py`, wired as `make opcodes`, output
committed at `tools/opcodes-v2.json`. No throwaway engine code; this file is
the write-up the issue's exit criteria ask for.

## Input caveat

The #2 parser-corpus slice and a vendored sqllogictest subset don't exist in
this repo yet (#2 is deferred pending spike #57's grammar-slice findings).
This run's 25 queries are hand-authored against an ad-hoc `products` table,
covering the V2 grammar slice (WHERE, ORDER BY, LIMIT/OFFSET, comparisons,
arithmetic, common scalar functions, DISTINCT, a scalar subquery). Treat this
as a first cut — re-run `make opcodes` once #2 lands to widen the input set.

## Result

**57 opcodes** harvested, against `plan.md:101`'s "~40 core opcodes" estimate
for V2 — about 40% over. See `tools/opcodes-v2.json` for full detail
(per-opcode count, p4 variants, example query, category).

## Surprises vs. the ~40 estimate

- **Aggregates showed up in a "no GROUP BY" query class.** `AggStep`/
  `AggFinal` appear from `SELECT * FROM products WHERE id = (SELECT max(id)
  FROM products))` — a scalar subquery using `max()`, no `GROUP BY` in
  sight. V2's phase table lists aggregates under V4 (`plan.md:147`), but a
  bare aggregate scalar subquery already needs `AggStep`/`AggFinal` at V2 if
  scalar subqueries are in scope. Worth an explicit decision: either scope
  scalar-subquery aggregates out of V2 too, or fold `AggStep`/`AggFinal`
  into the V2 opcode budget now.
- **DISTINCT pulls in ephemeral-table machinery.** `OpenEphemeral`,
  `Sequence`, `IdxInsert`, `Found`, `Delete` all appear from
  `SELECT DISTINCT note FROM products` — a full mini b-tree-backed dedup
  path, not just a flag on `ResultRow`. If DISTINCT is in the V2 grammar
  slice (it's listed as a top-level SELECT modifier), its opcode cost is
  closer to a second table implementation than a comparison tweak.
- **ORDER BY pulls in the full sorter opcode family**, not one or two ops:
  `SorterOpen`, `SorterInsert`, `SorterSort`, `SorterNext`, `SorterData`,
  `SorterCompare`(unused in this run but same family) — 5 distinct opcodes
  for what plan.md's table summarizes as one word ("Sort").
- **`RealAffinity` appears 27 times** — once per query touching a REAL
  column, independent of whether a comparison is actually happening. It's
  affinity coercion applied on load, not a comparison-time op — this maps
  awkwardly onto the issue's requested taxonomy (cursor/control/compare/
  arithmetic/function/sorter/result), which has no dedicated "coercion"
  bucket. Filed here under `compare` as closest fit; worth revisiting when
  spec 008 (value-semantics kernel) formalizes affinity handling.
- **LIMIT/OFFSET is three opcodes, not one**: `OffsetLimit` (setup),
  `IfNotZero`/`IfPos`/`DecrJumpZero` (the actual per-row counters) — the
  counting logic is control flow, not a dedicated LIMIT opcode.
- **`CollSeq` and `MustBeInt`** appear from ordinary comparisons/BETWEEN,
  not just from explicit `COLLATE` clauses — collation-sequence setup is
  emitted even for the default `BINARY` collation.

## Category counts (this run)

| Category | Count | Opcodes |
|---|---|---|
| cursor | 16 | Column, Delete, Found, IdxInsert, IdxLE, Last, Next, NullRow, OpenEphemeral, OpenPseudo, OpenRead, Prev, Rewind, Rowid, SeekRowid, Sequence |
| control | 15 | BeginSubrtn, DecrJumpZero, Goto, Halt, IfNot, IfNotZero, IfPos, Init, IsNull, MustBeInt, NotNull, OffsetLimit, Once, Return, Transaction |
| compare | 6 | CollSeq, Ge, Gt, Le, Lt, RealAffinity |
| arithmetic | 5 | Add, Divide, Multiply, Remainder, Subtract |
| function | 3 | AggFinal, AggStep, Function |
| sorter | 6 | Sort, SorterData, SorterInsert, SorterNext, SorterOpen, SorterSort |
| result | 6 | Copy, Integer, MakeRecord, Null, ResultRow, String8 |

(16+15+6+5+3+6+6 = 57, matching the total above. `Eq`/`Ne` didn't appear in
this run's queries — no `=`/`<>` comparison was tested — a gap in this
first-cut query set, not evidence they're unneeded.)

(Exact per-opcode counts and every category assignment: `tools/opcodes-v2.json`.)

## Growth-path note

Every surprise above is additive (more opcodes than guessed), not
structural — no finding here suggests the V2 slice needs restructuring,
only that its opcode budget in `plan.md` should be updated from "~40" to
account for DISTINCT/sorter/limit-counter machinery once the actual V2
query set (post-#2/#57) is known.

## Exit criteria

- [x] Harvested opcode inventory committed (`tools/harvest_opcodes.py`,
      reproducible against the pinned oracle declared in `Cargo.toml`'s
      `[package.metadata.oracle]`)
- [x] Surprises vs. the ~40 estimate documented (above)
- [ ] Phase-3 VDBE ticket scoped from the JSON — deferred to when phase 3
      starts, per epic #56's "tickets created as their phase starts" policy
