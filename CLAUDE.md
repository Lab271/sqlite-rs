# sqlite-rs

A binary-compatible Rust replication of SQLite. See `AGENTS.md` for the
worktree/agent-branch workflow.

## Token spend policy

Every ticket (GitHub issue) must secure its token spend up front and account
for it on close:

- **Before starting work** (`/take`), the issue must carry an explicit
  complexity/effort estimate (the `## Complexity` section produced by
  `/issue`). Treat that estimate as the token budget for the ticket — if the
  work is ballooning past it, stop and re-scope with the user rather than
  quietly continuing.
- **Large or multi-agent work** (via the `Workflow` tool) must pass an
  explicit `budget.total` (from a user "+Nk tokens" directive, or a sane
  default sized to the ticket's complexity estimate) so spend is capped, not
  open-ended.
- **On close** (PR description or issue closing comment), note actual spend
  or effort relative to the estimate — one line is enough (e.g. "spend:
  matched estimate" or "spend: 2x estimate because X"). This keeps AI usage
  cost visible per ticket instead of only visible in aggregate.

This applies to every ticket, not just large ones — trivial tickets just get
a trivial budget.
