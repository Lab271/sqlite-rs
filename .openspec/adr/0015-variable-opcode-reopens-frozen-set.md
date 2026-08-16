# 0015 — `Variable` reopens the frozen V2 opcode set

**Status:** Accepted · **Date:** 2026-08-16

## Context

`tools/opcodes-v2.json` is the oracle-harvested, scope-frozen inventory of
opcodes V2 codegen is allowed to emit (#87 froze it at 60; #139's
bitwise/concat harvest grew it once already, so "frozen" has always meant
"frozen except by a deliberate re-harvest," not "immutable"). `src/vdbe/
program.rs`'s `Opcode` enum and `tests/vdbe/opcode_completeness_test.rs`
enforce that inventory as a hard boundary: an opcode outside it fails the
build.

#137 ("codegen emits `SeekRowid` for `WHERE rowid = <const>`") was scoped
as a pure codegen pattern-match — `SeekRowid` itself was already
implemented (#90) and already in the frozen set. But the issue's stated
in-scope shape included `WHERE rowid = ?`, and V2 had no way to make that
correct: `ExprKind::Param` compiled to an allocated-but-never-written
register (always NULL at runtime) because no bind-value API existed
anywhere in the VM. Emitting `SeekRowid` for a `?` operand without fixing
that would ship a feature that silently always seeks rowid NULL — worse
than not having the feature.

## Decision

Re-run the harvest (`tools/harvest_opcodes.py`, pinned oracle 3.53.4) with
a `WHERE id = ?1` query added to the V2 query set. The oracle's own plan
for that query includes `Variable` (SQLite's real bound-parameter opcode)
in the same `Integer`/`SeekRowid`-shaped point lookup — so `Variable`
joins the frozen set as opcode 61, not a hand-invented addition. Alongside
it: `Vm::bind_params`/`Vm::param` (`src/vdbe/exec.rs`), a `variable` exec
handler (`src/vdbe/result.rs`), and `RegAlloc::anonymous_param`/
`numbered_param` (`src/codegen.rs`) to assign 1-based parameter indices
during codegen. Only `?` and `?NNN` are wired to `Variable` — named forms
(`:name`/`@name`/`$name`) still compile to the old always-NULL stub,
because nothing in this ticket's scope needed them indexed.

## Alternatives rejected

- **Descope `?` to a follow-up ticket, ship `<integer literal>` only.**
  The safer, smaller change — flagged to the user as the recommended
  option before starting. Rejected per explicit user direction: the bind
  API was worth doing now rather than leaving `WHERE rowid = ?` as a
  documented gap in an otherwise-shipped feature.
- **Pattern-match the `?` shape and emit `SeekRowid` reading the existing
  always-NULL register, without a real bind API.** Rejected outright: it
  compiles without error but is never correct at runtime — the worst kind
  of "supported."
- **Hand-add `Variable` to `opcodes-v2.json` without re-running the
  harvester.** Rejected: the whole point of the frozen-set discipline is
  that every opcode traces to an actual oracle `EXPLAIN` observation, not
  a hand-authored guess at what SQLite would do. Re-harvesting keeps that
  invariant intact instead of quietly weakening it.

## Consequences

- `Opcode::ALL` is now 61 entries; `tools/opcodes-v2.json`'s
  `opcode_count` and `query_count` both moved (60→61, 29→30).
  `tools/harvest_opcodes.py`'s `QUERIES` list permanently carries the new
  `WHERE id = ?1` case, so a future re-harvest keeps `Variable` in scope
  rather than needing to be re-added.
- `execute_with_params`/`execute_with_db_and_params` (`src/vdbe.rs`) are
  new public entry points alongside the existing `execute`/
  `execute_with_db` — additive, no existing signature changed.
  `Vm::bind_params` is the only mutation-capable addition to `Vm`'s public
  surface.
  - Named bind forms (`:name`/`@name`/`$name`) remain an always-NULL
    known simplification, same as before this ticket — a real name→index
    resolution table is future work if a ticket ever needs it.
