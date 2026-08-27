# sqlite-rs

`sqlite-rs` is a binary-compatible Rust replication of SQLite: it reads and
writes the exact same `.db`/`.sqlite` file format as the reference C
implementation.

This book gathers the project's specifications, architecture decisions, and
release plan in one place:

- **Architecture** — C4 diagrams and the four-tier layer model
- **Specs** (`.openspec/specs`) — numbered functional and cross-cutting
  specifications, each with RFC 2119 requirements and scenarios
- **ADRs** (`.openspec/adr`) — architectural decision records
- **Plan** (`.openspec/plan.md`) — the value-block roadmap (V1…V12)

Source: <https://github.com/iheitlager/sqlite-rs>
