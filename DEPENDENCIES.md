# Dependencies

sqlite-rs targets security-sensitive contexts (forensics, FRANK). Minimal
dependencies is a deliberate stance, not an oversight: every dependency is
a trust boundary, and proc macros in particular execute arbitrary code at
build time. As of #553, production code has **zero proc-macro
dependencies**.

This file is the audit trail for that stance — every direct dependency,
why it's here, and what was considered instead. Supply-chain policy
enforcement (license allow-list, duplicate-version bans, source
restrictions) lives in `deny.toml` (`make deny`); known-vulnerability
scanning runs via `make audit` (cargo-audit against the RustSec advisory
database). Both run in CI's `lint-and-supply-chain` job.

## Production dependencies

| Crate | Version | License | Purpose | Maintenance | Alternatives considered |
|-------|---------|---------|---------|-------------|--------------------------|
| [`nix`](https://crates.io/crates/nix) | 0.31 | MIT | POSIX file locking (`flock`, via the `fs` feature) for `src/vfs/lock.rs`'s cross-process database locking | Actively maintained, ~150 contributors, widely used across the Rust ecosystem | Hand-rolling raw `libc::flock` FFI — rejected: `nix` gives safe wrappers over the same syscalls with no proc-macro cost and no meaningful trust-surface increase over `libc` itself |
| [`rustyline`](https://crates.io/crates/rustyline) | 14 | MIT | REPL line editing and persistent history (#551) | Actively maintained, the de facto standard readline replacement in the Rust ecosystem | Hand-rolling raw terminal input handling — rejected as a large maintenance burden for a CLI convenience feature, not core engine functionality; a `--no-repl` build isolating this dependency was considered but the REPL is a first-class deliverable, not optional |

Neither crate pulls in a proc-macro dependency of its own (verify with
`cargo tree -e features` if that ever changes).

## Development-only dependencies

These never ship in the production binary/library and are not part of the
trust boundary for downstream consumers — `cargo build --release` (a
library or binary build) does not compile them. They're documented here
for completeness since they still appear in `Cargo.lock` and are covered
by `make deny`/`make audit`.

| Crate | Version | License | Purpose |
|-------|---------|---------|---------|
| [`proptest`](https://crates.io/crates/proptest) | 1 | MIT OR Apache-2.0 | Property-based tests under `tests/proptest/` |
| [`criterion`](https://crates.io/crates/criterion) | 0.8 | Apache-2.0 OR MIT | Performance benchmarking harness (`tests/performance/`, tier 1) |
| [`rusqlite`](https://crates.io/crates/rusqlite) | 0.32 | MIT | Oracle-diff comparisons against a pinned real SQLite build (corpus/parity suites); deliberately built without the `bundled` feature so it links the same pinned oracle version as the rest of the test suite, not a vendored one |
| [`md-5`](https://crates.io/crates/md-5) | 0.10 | MIT OR Apache-2.0 | MD5 hashing for sqllogictest's `"N values hashing to <md5>"` result-block format — a test-format requirement, not a project cryptographic dependency |

## Policy enforcement

- **License allow-list, duplicate-version bans, source restrictions:**
  `deny.toml`, enforced by `make deny` (cargo-deny) in CI.
- **Known-vulnerability scanning:** `make audit` (cargo-audit against the
  RustSec advisory database), enforced in CI.
- **Vendored/pinned supply-chain audit trail:** `cargo vet` (see
  `supply-chain/`), enforced in CI. Notably audited the transitive
  dependencies `rustyline` pulled in (#551).
- Updating this file is part of the review for any PR that adds, removes,
  or upgrades a direct dependency.
