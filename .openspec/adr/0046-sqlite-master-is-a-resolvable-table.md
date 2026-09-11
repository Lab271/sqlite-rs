# 0046: `sqlite_master`/`sqlite_schema` is a resolvable table, not a synthetic CLI result set

Date: 2026-09-11

## Context

#707: `SELECT ... FROM sqlite_master` didn't compile at all —
`cannot compile statement: unsupported: no such table: sqlite_master`.
`read_schema` decodes the catalog every statement compiles against,
but nothing made that catalog reachable as a queryable *table* from the
`SELECT` path, so a consumer had no way to discover what indexes exist
on a table (`Connection::table_names()`, added as a stopgap, only
covers table names).

Two routes were on the table:

1. Make `sqlite_master` a resolvable table — teach
   `resolve_from_table_schema` (`src/codegen/subquery/from_clause.rs`)
   about it, so `SELECT`/`WHERE`/`ORDER BY` all work on it exactly like
   any other table.
2. Add an index-listing method to the embedding API
   (`Connection::indexes(table)`), reading `TableSchema::indexes`
   directly — no compiler change at all.

ADR-0029 (introspection pragmas outside the VDBE) explicitly named
this fork in its "Consequences" section: it chose synthetic,
CLI-layer-only result sets for 9 fixed-shape pragmas specifically
*because* none of them needed real `WHERE`/`JOIN` composability at the
time, and said the virtual-table-style alternative should be revisited
"if a real need for querying pragma output as a table... arrives" —
with an explicit instruction to supersede, not edit, when that
happens.

## Decision

Route 1. `sqlite_master` (and its modern alias `sqlite_schema`) is a
real b-tree at a well-known root page (1), with a fixed five-column
shape (`type`, `name`, `tbl_name`, `rootpage`, `sql`) — not a
CLI-synthesized result set like the 9 pragmas ADR-0029 covers.
`resolve_from_table_schema` now recognizes the name and hands back a
hardcoded `TableSchema` rooted at page 1 instead of consulting the
decoded catalog (which never contains an entry for itself — it
describes the *objects* on page 1, not the page itself). Every other
part of codegen treats the result exactly like an ordinary table scan:
no new opcode, no synthesized rows, `WHERE type = 'index'` and
`ORDER BY name` fall out for free.

This supersedes ADR-0029's problem statement only for `sqlite_master`
specifically — the 9 read-only pragmas ADR-0029 covers
(`table_info`, `index_list`, etc.) are unaffected and still live at the
CLI layer; they were never in this ticket's scope (spec 013's
non-goals explicitly defer the rest of the introspection pragma
catalogue to plan.md V7).

Route 2 (an `indexes()` API method) was not added: route 1 subsumes it
for any consumer willing to write SQL, and per the ticket's own
framing, growing the compiler to handle a real table beats growing the
embedding API's surface for one more read-only accessor.

## Alternatives rejected

- **Route 2 alone** (an API-level `indexes()` method): smaller, but
  narrower — it only answers the one question the consumer asked
  about (indexes), while route 1 also makes `sqlite_master.sql`,
  `.rootpage`, and arbitrary `WHERE`/`JOIN` composition against the
  catalog available, matching what every other SQLite consumer already
  expects to be able to do.
- **Synthesizing `sqlite_master`'s rows in memory** (CLI/API-layer,
  same shape as ADR-0029's 9 pragmas) rather than resolving it as a
  literal table scan over its real root page. Rejected: `sqlite_master`
  is unlike the 9 pragmas in one load-bearing way — it already *is* an
  ordinary rowid table on disk, so treating it as one is less code, not
  more, and it makes the existing table-scan/`WHERE`/`ORDER BY`
  machinery apply automatically instead of needing its own filtering
  logic re-implemented.

## Consequences

- `resolve_from_table_schema` is the single choke point every `FROM`
  reference goes through (top-level `SELECT`, subqueries, `EXPLAIN
  QUERY PLAN`, the CLI's own ad hoc lookups) — recognizing the name
  once there covers all of them without hunting down each call site.
- `sqlite_stat1` remains out of scope (non-goal, already read
  internally for planning) and is not made resolvable by this change.
- A future ticket that wants `pragma_table_info('t')`-style queryable
  pragmas is still the virtual-table alternative ADR-0029 deferred —
  this ADR doesn't reopen that question, it only answers it for the
  one table that was already a real b-tree.
