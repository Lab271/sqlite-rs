# Contributing to sqlite-rs

sqlite-rs's primary objective is a memory-safe, drop-in alternative to
SQLite — not a new database engine. Every change should move it closer to
SQLite's actual behavior, not further away: the file format and SQL dialect
are the moat, so compatibility beats novelty.

That means development is built against a pinned oracle (a real `sqlite3`
binary) rather than from memory of the spec — every compatibility claim is
backed by a byte-level diff against that oracle (see `tests/corpus/`), not
by intuition. Most conventions below exist to keep that oracle-first
discipline intact — read `.openspec/` before proposing anything that changes
it.

Development proceeds in numbered value blocks (V1…V12), each moving from
working to working. See [.openspec/plan.md](.openspec/plan.md) for the full
breakdown of phases and what's landed so far.

## Getting started

```bash
git clone https://github.com/iheitlager/sqlite-rs.git
cd sqlite-rs
cargo build
make test
```

`make help` lists every available target with a one-line description. A few
you'll use often:

| Command | What it does |
|---|---|
| `make test` | Everything except the corpus oracle diffs (unit, public-API, proptest, doctests) |
| `make test-lib` | Just the library unit tests — fastest inner loop |
| `make test-corpus` | Fixture corpus / oracle harness against a pinned real `sqlite3` |
| `make test-tiers` | Tier 0–3 conformance suite (see `.openspec/specs/001-architecture`) |
| `make lint` | clippy + formatting |
| `make verify` | Full gate: coverage (`check-coverage`), supply-chain (`check-deny`), qualified-subset (`check-mvl-limit`), module layout (`check-mod-files`) |
| `make assurance` | Spec → code → test traceability dashboard |

`make test-corpus`, `make test-parity`, and `make test-sqllogictest` shell out
to a pinned `sqlite3` and are slower; they're not part of the default `cargo
test` run but are expected to pass before a PR touching engine behavior
merges.

## Architecture and specs

Design context lives in `.openspec/`, not in code comments:

- `.openspec/plan.md` — the value-block roadmap (V1…V12)
- `.openspec/specs/` — numbered functional specs (`NNN-name/spec.md`), each
  requirement traceable to an `Implementation:` path and `Tests:` links
- `.openspec/adr/` — Architectural Decision Records; check the index before
  proposing anything that contradicts an accepted ADR
- `.openspec/grammar/sqlite.ebnf` — the EBNF grammar the parser is built
  from, re-derived from a pinned `parse.y`

If your change touches parser grammar, run `make check-grammar-drift` before
committing. If it touches a spec requirement, add or update the requirement's
`Tests:` links — `make assurance` flags dead links.

## Code style

- **`#![forbid(unsafe_code)]`** at the crate root. Memory safety is the
  compiler's job here, not the test suite's — don't propose `unsafe` to work
  around a borrow-checker fight; redesign instead.
- `cargo fmt` for formatting; `cargo clippy` must be clean. The crate denies
  `unwrap_used`, `expect_used`, `indexing_slicing`, `panic`,
  `arithmetic_side_effects`, and `mod_module_files` at the lint level (see
  `Cargo.toml`'s `[lints.clippy]`) — these aren't suggestions, they're gates.
- No `foo/mod.rs` files — use `foo.rs` next to `foo/` (modern per-file module
  style). Enforced by `mod_module_files` and `make check-mod-files`.
- Every claim about SQLite-compatible behavior is backed by a byte-level diff
  against the pinned oracle, not by intuition — see `tests/corpus/`.

Run `make lint` and `make verify` locally before opening a PR; both run in CI.

## Commit messages

Conventional prefixes: `feat:`, `fix:`, `chore:`, `refactor:`, `test:`,
`docs:`, `bench:`. Reference the issue being closed where applicable, e.g.
`feat: WAL writer path (#389)`.

## Pull requests

- Keep PRs scoped to one issue/ticket where possible.
- Include a test plan; new behavior needs test coverage, bug fixes need a
  regression test.
- If your change touches an existing spec requirement, update its `Tests:`
  links in the same PR (`.openspec/specs/`).
- If your change closes an architectural alternative, add an ADR
  (`.openspec/adr/NNNN-title.md`) in the same PR.
- CI runs `make verify`, `make lint`, and the spec/grammar/version gates
  (`make check-assurance`, `make check-grammar-drift`, `make version-pin`) — a PR
  that doesn't pass these won't merge.

## Reporting bugs and requesting features

Use GitHub Issues. For security vulnerabilities, see
[SECURITY.md](SECURITY.md) instead of opening a public issue.
