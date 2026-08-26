# Dependencies

sqlite-rs targets security-sensitive contexts (forensics, FRANK). Minimal
dependencies is a deliberate stance, not an oversight: every dependency is
a trust boundary, and proc macros in particular execute arbitrary code at
build time. As of #553, production code has **zero proc-macro
dependencies**; as of #558, the CLI's line editor is also hand-rolled; as
of #563, the `nix` crate's `fcntl`/`termios` FFI is vendored into
`src/sys/` (see [ADR-0030](.openspec/adr/0030-zero-proc-macro-dependencies.md)
and [ADR-0031](.openspec/adr/0031-vendor-nix-subset.md)). **sqlite-rs now
has zero production dependencies.**

This file is the audit trail for that stance — every direct dependency,
why it's here, and what was considered instead. Supply-chain policy
enforcement (license allow-list, duplicate-version bans, source
restrictions) lives in `deny.toml` (`make deny`); known-vulnerability
scanning runs via `make audit` (cargo-audit against the RustSec advisory
database). Both run in CI's `lint-and-supply-chain` job.

## Production dependencies

None. `sqlite-rs` has zero external production dependencies.

POSIX byte-range file locking (`src/vfs/lock.rs`'s cross-process database
locking) and raw-mode termios (the CLI's hand-rolled readline, #558) both
need syscalls no pure-Rust/`std` API covers. Rather than depend on `nix`
(safe wrappers) or `libc` (raw FFI + ABI structs) for those, #563 vendors
the ~180 lines actually needed — hand-written `unsafe extern "C"` bindings
plus the per-platform `struct flock`/`struct termios` ABI layouts (macOS
and Linux, verified against each platform's own headers) — into
`src/sys/`. See [ADR-0031](.openspec/adr/0031-vendor-nix-subset.md) for
the rationale and what was rejected. `rustyline` (formerly listed here,
#551) was replaced by a hand-rolled readline in
`src/bin/sqlite-rs/readline/` (#558) — see
[ADR-0030](.openspec/adr/0030-zero-proc-macro-dependencies.md).

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
- **Reviewed lockfile updates:** `make update` runs `cargo update`, then
  re-runs `make deny`/`make audit` against the new `Cargo.lock` before you
  commit it, so a bad transitive bump surfaces before merge, not after.
- **Local vendor inspection:** `make vendor` runs `cargo vendor vendor/`
  for reading the exact upstream source of every dependency (including
  transitive ones) on disk; `vendor/` is gitignored and not built from by
  default — this is an inspection aid, not the build's source of truth.
- Updating this file is part of the review for any PR that adds, removes,
  or upgrades a direct dependency.
