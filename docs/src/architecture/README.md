# Architecture Overview

`sqlite-rs` is organized as four tiers, from raw file I/O up to the public
API. Each tier only depends on the tiers below it.

- [System context](c4-context.md) — where sqlite-rs sits relative to callers
  and the database file
- [Container view](c4-container.md) — the four tiers and their components
- [Tiering](tiering.md) — what each tier is responsible for, and the tier
  contract model (see `.openspec/specs/001-architecture`)
- [Data flow](data-flow.md) — how a query moves through the tiers
