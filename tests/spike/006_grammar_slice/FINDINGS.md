# Spike 006 — grammar-slice viability for SELECT core

Issue #57, part of epic #56 (V2). Refs: 002/Req-2.

## Question

Spike 001 chose the parser toolchain (pomelo — see `tests/spike/001_parser/comparison.md`).
It didn't answer the *slicing* question: can the V2 SELECT-core subset be carved
out of the full grammar cleanly, reject out-of-subset SQL with a diagnostic
distinct from "invalid SQL", and grow to V3 by *adding* rules rather than
restructuring?

## What was built

`tests/spike/006_grammar_slice/` — a standalone pomelo crate containing
*only* the `.openspec/grammar/sqlite.ebnf` `(* V2 *)`-tagged rules: single-FROM
`select`, `WHERE`, `ORDER BY`, `LIMIT`/`OFFSET`, and the V2 expression grammar
(arithmetic/logical/comparison/concat, `BETWEEN`, `IN (...)`, `LIKE`,
column refs, scalar function calls). No `CREATE TABLE`/`INSERT`/`UPDATE`/
`DELETE` (V3) and no `GROUP BY`/`HAVING`/joins/subqueries/compound-select (V4)
productions exist in the grammar at all — that absence is the thing under
test, not a placeholder.

`BETWEEN`/`IN`/`LIKE` were added beyond spike 001's expr grammar deliberately:
their `NOT`-prefixed forms and `BETWEEN`'s own use of the `AND` keyword (which
already has a top-level precedence meaning) are exactly the kind of LALR
slicing trap the issue calls out.

Three-way outcome classification (`Outcome::{Accepted, Unsupported,
SyntaxError}` in `src/lib.rs`) works by trying the sliced grammar first; on
failure, a keyword sniff (`src/unsupported.rs`) checks for known out-of-slice
markers (`INSERT`/`UPDATE`/`DELETE`/`CREATE`/`DROP`/`WITH`, or `JOIN`/`UNION`/
`INTERSECT`/`EXCEPT`/`GROUP`/`HAVING`/a second `SELECT` inside a SELECT) and
upgrades the diagnostic to "unsupported: `<feature>` (V3/V4)"; anything else
that fails to parse is a genuine syntax error.

## Results against the exit criteria

- [x] **Slice parses the corpus subset with 3-way outcome parity.**
  `tests/fixtures.rs` runs all three fixture files
  (`fixtures/{v2_valid,unsupported,invalid}.sql`) through `parse()`: 22 V2
  statements accept, 11 out-of-slice statements (INSERT/UPDATE/DELETE/CREATE/
  DROP/JOIN/GROUP BY/UNION/subquery/CTE) come back `Unsupported`, 9 malformed
  statements come back `SyntaxError`. `cargo test` — 4/4 green, no LALR
  conflicts reported by pomelo at macro-expansion time (checked by touching
  `grammar.rs` and grepping build output for `conflict`/`warning`).
- [x] **Growth-path finding.** Sketched adding V3's `INSERT` statement back
  into the slice (see below) — 20 pure-addition lines (new `insert`,
  `insert_cols`, `id_list`, `row_list` nonterminals + 3 new tokens in the
  tokenizer) plus 2 *modified* lines: `stmt`'s type annotation
  (`Select` → `Stmt`, an enum wrapping both) and its one existing alternative's
  action (`{ S }` → `{ Stmt::Select(S) }`, to wrap the payload). No existing
  `select`/`expr`/precedence rule needed to change; it compiled clean, no new
  conflicts.
- [x] **GO/NO-GO + recommendation** — see below.

## Growth-path probe detail

Copied the slice, added:

```
%type stmt Stmt;                              // was: %type stmt Select;
stmt ::= select(S) { Stmt::Select(S) }        // was: stmt ::= select(S) { S }
stmt ::= insert(S) { S }                      // new alternative

%type insert Stmt;
insert ::= Insert Into Id(T) insert_cols(C) Values row_list(R) { ... }
%type insert_cols Vec<String>;
insert_cols ::= { Vec::new() }
insert_cols ::= LParen id_list(L) RParen { L }
%type id_list Vec<String>;
id_list ::= Id(A) { vec![A] }
id_list ::= id_list(mut L) Comma Id(A) { L.push(A); L }
%type row_list Vec<Vec<Expr>>;
row_list ::= LParen expr_list(E) RParen { vec![E] }
row_list ::= row_list(mut L) Comma LParen expr_list(E) RParen { L.push(E); L }
```

plus 3 new keyword tokens in the tokenizer (`INSERT`, `INTO`, `VALUES`) and an
`ast::Stmt` enum wrapping `Select`/`Insert`. `cargo build` succeeded with zero
conflicts. This mirrors exactly what `tests/spike/001_parser/002_pomelo`
already proves at full scale (its `grammar.rs` implements CREATE
TABLE/INSERT/SELECT/UPDATE/DELETE together, also conflict-free) — the slice
doesn't lose that property by having fewer productions today.

## LALR slicing traps checked

- `BETWEEN ... AND ...` vs. top-level `expr AND expr`: both use the `AND`
  token but at different grammar positions. Resolved cleanly with
  `%nonassoc Between In Like;` sitting between the comparison and additive
  precedence tiers, mirroring parse.y's placement. No conflict.
- `NOT BETWEEN` / `NOT IN` / `NOT LIKE` vs. the prefix `Not expr` rule and
  `%right Not`: resolved without conflict — pomelo's LALR(1) tables
  disambiguate on the token immediately following `NOT` (`Between`/`In`/
  `Like` vs. the start of another `expr`).
- Removing GROUP BY/HAVING and all DML/DDL productions entirely (rather than
  keeping them as unreachable dead rules) did not introduce any conflict
  either — the slice's reduced production set is simply smaller, not
  differently shaped.

No conflicts were found under any of the above. This is a case where the
*absence* of a conflict is itself informative: it directly answers "does
removing productions from a shared grammar shape create LALR conflicts that
adding them wouldn't."

## GO / NO-GO

**GO.** The hypothesis holds:

- (a) The sliced grammar accepts exactly the V2 subset.
- (b) Unsupported vs. invalid is distinguishable — via a coarse keyword sniff
  layered on top of the parser, not via the parser's own error variants (the
  parser only ever emits `SyntaxError`; classification into `Unsupported`
  happens one layer up). This is a deliberate design choice worth carrying
  into phase 1: **the sliced parser stays a pure "V2 grammar or bust" LALR
  parser; the tokenizer/parser API layer above it does the out-of-slice
  classification.** That keeps growth (b) simple: each V-block just adds its
  own keyword markers to the classifier, no grammar changes needed purely for
  better diagnostics.
- (c) Growth to V3 is additive-only, confirmed by an actual sketch, not just
  argument.

**Recommendation for the phase-1 parser ticket (#61):** proceed with pomelo,
sliced to exactly the V2 productions in `.openspec/grammar/sqlite.ebnf`, using
this spike's `Outcome` split (grammar-level accept/reject + a layer-above
keyword classifier for "unsupported") as the shape for the real
tokenizer/parser boundary. No hand-written recursive descent fallback needed.

## Scope not covered (explicitly out of timebox)

`CASE`/`CAST`/bind parameters (`?`, `:name`, `@name`, `$name`) are V2-tagged in
`sqlite.ebnf` but were not added to this spike's grammar — they're pure
`primary_expr` alternatives with no interaction with the precedence table or
the BETWEEN/IN/LIKE additions already tested, so they carry materially lower
slicing risk than what was tested here. Phase 1 (#61) should add them
directly; no further spike needed.

## Spend

Estimate: 1-2 day timebox (no formal `## Complexity` section on the issue).
Actual: single AI session, well within timebox — spend roughly matched a
"small" ticket once research (existing spike 001/pomelo code, `.openspec/grammar`)
was accounted for.
