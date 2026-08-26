# 0031 — Vendor a `nix` subset: reintroduce a single, narrow `unsafe` boundary

**Status:** Accepted · **Date:** 2026-08-26 (partially supersedes ADR-0009)

## Context

ADR-0009 eliminated this crate's `unsafe` entirely (#66) by moving the
remaining raw `fcntl`/`mmap`/`fork` calls onto `nix`'s safe wrappers and
`std`, and declared `#![forbid(unsafe_code)]` crate-wide — "zero unsafe in
this codebase" as a machine-checked (`mvl-limit`, `cargo geiger`) claim.

That claim was always narrower than "zero external trust": `nix` still
carries the actual `unsafe extern "C"` FFI, plus its own transitive deps
(`libc`, `cfg-if`, `bitflags`) and a `build.rs`. For a forensics/FRANK
positioning where the external-dependency supply chain (typosquatting, a
compromised maintainer, a malicious update) is the threat model that
matters most, "zero unsafe in our own source" and "zero external trust"
are different guarantees, and #563 is what this crate needs: the second
one.

## Decision

Vendor the ~180 lines of `unsafe extern "C"` FFI `nix::fcntl` and
`nix::sys::termios` actually provided (byte-range `fcntl(F_SETLK/
F_GETLK/F_SETLKW)` locking and `tcgetattr`/`tcsetattr`/`cfmakeraw`/
`isatty` raw-mode terminal control) into `src/sys/`, hand-writing the
platform ABI (`struct flock`, `struct termios`, and their constants) for
macOS and Linux from each platform's own headers rather than depending on
the `libc` crate for it. Remove `nix` from `Cargo.toml` — the crate now
has zero external dependencies.

This reintroduces exactly one `unsafe` boundary: `src/lib.rs`'s
`#![forbid(unsafe_code)]` becomes `#![deny(unsafe_code)]`, with a scoped
`#![allow(unsafe_code)]` in `src/sys/` only (`make mvl-limit`'s qualified
subset gains that one exemption). Every other module — `src/vfs/lock.rs`,
`src/vfs/shm.rs`, `src/vfs/test_lock_probe.rs` included, ADR-0009's actual
subject — stays exactly as unsafe-free as it was: they call
`crate::sys::fcntl`/`crate::sys::termios`'s *safe* wrapper functions, the
same call shape as the `nix` APIs they replace.

## Alternatives rejected

- **Depend on the `libc` crate instead of vendoring.** Removes `nix`'s
  safe-wrapper layer but keeps an external dependency (and `libc`'s own
  supply chain) for the ABI structs/constants this ADR hand-writes
  instead — doesn't reach #563's "zero external dependencies" goal.
- **Keep `#![forbid(unsafe_code)]` and find a pure-Rust alternative.**
  No pure-Rust or `std` API exists for POSIX byte-range record locks or
  raw-mode termios — both `std::fs`'s locking and `std::io`'s terminal
  handling stop short of what SQLite's locking protocol and readline's
  raw keypress input need. The FFI boundary is unavoidable, not merely
  `nix`'s implementation choice.

## Consequences

- Zero external dependencies (no `nix`, and no `libc` crate pulled in
  through it) — the only trust boundary left is the Rust compiler and the
  two platforms' own libc/kernel ABIs, which every Rust binary already
  links against and trusts regardless.
- `#![forbid(unsafe_code)]` → `#![deny(unsafe_code)]`: ADR-0009's "zero
  unsafe, no override possible anywhere" claim is narrowed to "zero
  unsafe outside one audited, `pub(crate)`-scoped module" — a real but
  deliberate loosening, not a regression on ADR-0009's actual
  motivation (getting `unsafe` out of `src/vfs/`'s locking/mmap logic,
  which stands unchanged).
- `src/sys/`'s FFI is exercised by SQLite's real locking test suite
  (`src/vfs/lock.rs`, `src/vfs/shm.rs`'s subprocess-contention tests) plus
  direct unit tests in `src/sys/fcntl.rs`/`src/sys/termios.rs` — the ABI
  structs are load-bearing for every one of those, so a layout mistake
  fails loudly rather than silently.
