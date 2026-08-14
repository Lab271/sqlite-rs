# Fixture corpus

Quick orientation — the design and requirements live in
[`.openspec/specs/004-corpus/spec.md`](../../.openspec/specs/004-corpus/spec.md).

- **Regenerate:** `make fixtures` (needs a pinned, non-codec `sqlite3`; override with `ORACLE_SQLITE3=/path/to/sqlite3`)
- **Run the harness:** `make test-corpus`
- **Fixtures:** committed under `fixtures/<family>/`, each family with a `manifest.txt` describing its members — `serialtypes/`, `encodings/`, `pagesizes/`, `btrees/`, `features/`, `invalid/`
- **Not here yet:** WAL-pending and hot-journal fixtures, split out to #21

The harness (`main.rs`, `oracle.rs`, `harness.rs`, `*_test.rs`) reads
committed fixtures directly — running it never depends on `sqlite3` being
installed. Only `make fixtures` (regeneration) needs the pinned oracle.
