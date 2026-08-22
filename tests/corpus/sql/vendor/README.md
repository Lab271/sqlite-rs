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

## `sqllogictest/` — 14 files

From the sqllogictest suite, `gregrahn/sqllogictest` mirror pinned at commit
`c67f97bf3ca7e590d12e073408bcacaf2ff0f3a0`.

The upstream suite is 110 MB / 699 files / ~7.2M queries and is not committed.
Vendored here are the 12 hand-written `test/evidence/*.test` files (which carry
the `EVIDENCE-OF:` requirement tags and cover DDL/DML statement types) plus
`test/select1.test` and `test/select2.test`. Excluded: `select3/4/5.test` and
every `test/random/**` and `test/index/**` file, which are generated output of
1–4 MB *each* and enormously repetitive.

Why the mirror and not sqlite.org: the canonical corpus lives in a Fossil repo
at <https://www.sqlite.org/sqllogictest/>, whose tarball endpoint returns an
HTML anti-robot page with a `200` status. It cannot be fetched by tooling or
CI, so the pin tracks the GitHub mirror instead.

## `tcl/` — 43 files

From SQLite's own TCL test suite, `test/*.test` in the SQLite source tree at
tag `version-3.53.3` — matching `[package.metadata.oracle]` in `Cargo.toml`, so
extracted SQL lines up with the `sqlite3` the corpus is validated against.

The upstream suite is 1189 files; vendored here is a selection covering every
statement category. The DML/DDL selection is deliberately broader than the
SELECT selection: sqllogictest is a *query* suite whose DML is incidental
setup, so this suite is where INSERT/UPDATE/DELETE/DDL diversity actually comes
from.

## Licensing

Both corpora are public domain, consistent with SQLite itself.
