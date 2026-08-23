# Security Policy

## Project status

Read this before deciding how much to trust anything here.

sqlite-rs is a young, actively-developed reimplementation of the SQLite file
format and SQL dialect in Rust. It has **not had an independent security
review**, and much of it is written by AI agents under human review (see
`AGENTS.md`). `#![forbid(unsafe_code)]` and the crate's clippy lint gates
(`unwrap_used`, `indexing_slicing`, `arithmetic_side_effects`, etc. — see
`CONTRIBUTING.md`) rule out memory-unsafety by construction, but **memory
safety is not correctness**: a safe program can still read or write the wrong
bytes. Every compatibility claim is backed by a byte-level diff against a
pinned real `sqlite3` (see `tests/corpus/`), not by the type system alone —
check `make assurance` for what's actually covered before relying on this for
anything security-sensitive.

## Supported versions

Only the latest released minor version is supported. There are no security
backports to older versions; fixes ship in the next release.

## Reporting a vulnerability

**Do not open a public GitHub issue for a security report.**

Please use [GitHub's private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing/privately-reporting-a-security-vulnerability)
via this repository's **Security** tab.

Include, where possible:

- what the vulnerability is and its impact
- steps to reproduce, ideally a minimal `.sql`/file input
- affected version and platform
- any known workaround

## Response targets

| Stage | Target |
|---|---|
| Acknowledgement | 5 business days |
| Confirmation or closure | 15 business days |

These are targets, not contractual commitments — this is not a project with a
staffed on-call rotation.

## Scope

**In scope:**

- memory-safety or undefined-behavior issues (should be impossible under
  `forbid(unsafe_code)`, but a soundness hole in the toolchain or an
  unintentional `unsafe` block would be reportable)
- crashes, hangs, or unbounded resource growth triggered by malformed SQLite
  database files or SQL input
- corruption of a database file that could be induced by an attacker-supplied
  input
- CI/release supply-chain issues (compromised publish path, dependency
  confusion)

**Out of scope:**

- correctness bugs with no security impact — file those as regular issues
- behavior differences from real `sqlite3` with no security impact
- vulnerabilities in upstream dependencies, unless our use of them is what
  creates the exposure (report those upstream; tell us too if we should pin
  or drop the dependency)
- scanner output with no demonstrated impact

## Disclosure

We prefer coordinated disclosure: we'll agree a timeline with you, credit you
in the release notes unless you ask otherwise, and publish a fix before
public detail.
