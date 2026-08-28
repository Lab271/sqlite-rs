# Vendored external test-suite subsets

Source material for the extracted SQL corpus (issue #70). These files are
**verbatim copies** of upstream test files, kept here so the corpus is
regenerable offline with traceable provenance. Nothing here is hand-edited.

Regenerate the extraction from these files:

```sh
make extract-sql-corpus
```

Refresh these files from upstream (requires network):

```sh
make extract-sql-corpus FETCH=1
```

## `sqllogictest/` — 17 files

From the sqllogictest suite, `gregrahn/sqllogictest` mirror pinned at commit
`c67f97bf3ca7e590d12e073408bcacaf2ff0f3a0`.

The upstream suite is 110 MB / 699 files / ~7.2M queries and is not committed.
Vendored here are the 12 hand-written `test/evidence/*.test` files (which carry
the `EVIDENCE-OF:` requirement tags and cover DDL/DML statement types) plus
`test/select1.test` through `test/select5.test` — the hand-written query files
that stay a sane size (each well under 1.2 MB) and push past the V2
single-table slice into the joins/subqueries/aggregates `select3-5.test`
exercise (V4, `.openspec/plan.md`). Excluded: every `test/random/**` and
`test/index/**` file (605 files), which are generated output 1–4 MB *each* and
enormously repetitive — taking the first N would yield N near-identical
queries, not N structurally different ones.

Why the mirror and not sqlite.org: the canonical corpus lives in a Fossil repo
at <https://www.sqlite.org/sqllogictest/>, whose tarball endpoint returns an
HTML anti-robot page with a `200` status. It cannot be fetched by tooling or
CI, so the pin tracks the GitHub mirror instead.

## `tcl/` — 60 files

From SQLite's own TCL test suite, `test/*.test` in the SQLite source tree at
tag `version-3.53.4` — matching `[package.metadata.oracle]` in `Cargo.toml`, so
extracted SQL lines up with the `sqlite3` the corpus is validated against.

The upstream suite is 1159 files; vendored here is a selection covering every
statement category, widened past the original V2/V3 single-table slice up
through V7 per `.openspec/plan.md`'s own corpus citations:

- **V4** (joins/subqueries/aggregates): `join2/3.test`, `subquery.test`,
  `subquery2.test`, `select6-8.test`, `aggnested.test`.
- **V6/V7** (CTEs — non-recursive at V6, recursive deferred to V7):
  `with3-6.test`, alongside the original `with1/2.test`.
- **V7** (transactions/pragmas/introspection): `savepoint.test`,
  `savepoint2.test`, `pragma.test`, `pragma2.test`, `analyze.test`.

Excluded, deliberately: V8+ files whose feature isn't implemented yet
(`fkey*.test`, `trigger1.test`/`trigger[3-9].test`, `window*.test`,
`gencol*.test`, `without_rowid*.test`, `strict*.test`) — vendoring ahead of
the engine's own roadmap would just inflate the "uncategorized"/"expected
error" drop counts in `make extract-sql-corpus`'s report without adding
signal.

The DML/DDL selection is deliberately broader than the SELECT selection:
sqllogictest is a *query* suite whose DML is incidental setup, so this suite
is where INSERT/UPDATE/DELETE/DDL diversity actually comes from.

## Licensing

Both corpora are public domain, consistent with SQLite itself.
