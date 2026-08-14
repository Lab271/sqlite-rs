# Spike findings — #8 / 005_locking_interop

Throwaway single-crate experiment (see `src/main.rs`, `src/lock.rs`,
`src/wal_shm.rs`, `src/harness.rs`), following 004_wal_reading's precedent
of a disposable, self-contained crate. Run with `cargo run` (or `make run`)
inside this directory; requires a stock `sqlite3` binary on `PATH`.

**Numbering note:** the issue's own title says "spike: 004 locking protocol
interop"; `tests/spike/004_wal_reading/` already occupies that slot (a
concurrent agent's WAL-reading spike, #7). Following that same commit's
precedent (and `003_csv_export`'s before it), this spike took the next free
disk slot, `005`, and this note documents the renumbering. `.openspec/plan.md`'s
Concurrency Contract section refers to this work as "spike 004" — read that
as this spike (005 on disk), not `004_wal_reading`.

**Scope constraint:** this sandbox is macOS-only (Darwin arm64). The issue
asks for both macOS and Linux exercise; only macOS was exercised here. Linux
POSIX lock semantics are believed compatible but are *not* independently
verified by this spike — flagged as an open item below, not silently dropped.

## Hypothesis: SURVIVES

A Rust process taking byte-identical `fcntl` locks — at the exact byte
offsets SQLite itself uses — genuinely interoperates with a live, stock
`sqlite3` CLI process, in both directions, for both journal-mode locking and
WAL-mode `-shm` reader-slot locking. All five experiments pass. One
significant methodology finding (below) and one real, hypothesis-relevant
surprise (WAL reset semantics) came out of getting experiment 4 right.

Byte offsets were verified against SQLite's actual source
(`github.com/sqlite/sqlite`, `src/os_unix.c` and `src/wal.c`), not
recollection:

| Constant | Value | Source |
|---|---|---|
| `PENDING_BYTE` | `0x40000000` (1073741824) | os_unix.c:7275 comment block |
| `RESERVED_BYTE` | `PENDING_BYTE+1` | os_unix.c:7276 |
| `SHARED_FIRST` | `PENDING_BYTE+2` | os_unix.c:7276 |
| `SHARED_SIZE` | 510 | os_unix.c:7276 (`SHARED_RANGE 0x...02 -> 0x...200`) |
| `UNIX_SHM_BASE` | 120 | os_unix.c:4602, asserted at os_unix.c:8549 |
| WAL lock bytes | WRITE=120, CKPT=121, RECOVER=122, READ(0..4)=123–127, DMS=128 | os_unix.c:8551-8558 |
| `WalIndexHdr` / `WalCkptInfo` layout | mxFrame @16 (48-byte hdr ×2), nBackfill @96, aReadMark[5] @100–119, aLock[8] @120–127 | wal.c:321-333, 388-395 |

## Exit criteria

- [x] Experiment 1 (reader blocks writer) — passes
- [x] Experiment 2 (writer blocks reader) — passes
- [x] Experiment 3 (PENDING semantics) — passes
- [x] Experiment 4 (WAL read-lock slot vs. checkpointer) — passes, after
      fixing a harness confound (finding 1)
- [x] Experiment 5 (close() trap + fd-cache-shaped workaround) — passes
- [x] macOS exercised — yes. Linux — **not exercised**, open item.
- [x] Findings written up (this file)
- [ ] Concurrency Contract section in plan.md validated/corrected — see
      "Consequences" below; not yet edited into plan.md itself.

## Results

```
[PASS] 1. reader (our SHARED lock) blocks stock sqlite3 writer
       — our lock=Acquired, sqlite3 insert ok=false, stderr="Error: stepping, database is locked (5)"
[PASS] 2. stock sqlite3 EXCLUSIVE blocks our SHARED read attempt
       — our shared-lock attempt while sqlite3 held EXCLUSIVE: Blocked
[PASS] 3. PENDING (held by stock sqlite3) refuses a brand-new SHARED reader
       — our shared-lock=Acquired, new-reader probe while sqlite3 mid-COMMIT retry=BLOCKED, sqlite3 exit ok=true
[PASS] 4. our WAL read-lock slot + mark makes sqlite3's checkpointer back off
       — mark held=Acquired, mxFrame claimed=3, slot=2,
         wal_size before_ckpt=20632 while_blocked(busy=1)=20632, after_release(busy=0)=0
[PASS] 5. close()-drops-all-locks trap, and the fd-cache-shaped workaround
       — trap: our lock=Acquired, external probe after unrelated close()=ACQUIRED (trap reproduced)
       — workaround: our lock=Acquired, external probe without a real close()=BLOCKED (lock survived)
```

Stable across repeated runs (checked 3x consecutively, no flakiness observed
in this environment; experiment 3 has a fixed 200ms sleep to let sqlite3
enter its busy-retry loop before probing — see finding 3, a timing
assumption worth hardening if this spike's code is ever reused rather than
thrown away).

## Findings

1. **A one-shot `sqlite3 db "SQL..."` CLI invocation is its own connection,
   and closing the last connection to a WAL-mode database fully
   checkpoints (and can reset) the WAL — independent of any lock a
   *different* process holds.** First implementation of experiment 4 used
   one-shot `sqlite3` calls for both the initial fixture and the "extra
   writes" step. Observed `mxFrame` *decreasing* (3 → 2) after two more
   inserts, which looked at first like our WAL-reader lock had failed to
   block a writer-triggered WAL reset (`walRestartLog` in wal.c). Diagnostic
   probes (a genuine second OS process attempting the *exact* single-byte
   and 4-byte-range exclusive locks `walRestartLog` itself takes over
   `WAL_READ_LOCK(1..4)`) confirmed our held SHARED lock **did** correctly
   block both — i.e., the primitive byte-range lock was correctly
   interoperable the whole time. The real cause was that each one-shot
   `sqlite3` invocation is a fresh connection; when *it* closes (being the
   last connection at that moment), SQLite's close-time full checkpoint
   fires regardless of what a separate process's `-shm` lock is doing —
   this checkpoint is not gated on the `WAL_READ_LOCK` slots our lock
   protects, only WAL-level reset/rewrite-from-scratch situations are. This
   directly echoes 004_wal_reading's own finding 1 ("closing the last
   connection ... fully checkpoints it, even with `wal_autocheckpoint=0`").
   **Fix:** use a persistent `sqlite3 -batch` session (kept open across the
   whole experiment) as the writer, matching how a real application holds
   its connection open — this is not just a workaround but the *more
   realistic* shape for the question the experiment actually asks. After
   the fix, `mxFrame` progresses monotonically (3 → 5, a clean append, no
   reset) and the checkpointer visibly backs off (`busy=1`, WAL stays at
   20632 bytes) while our lock+mark are held, then proceeds cleanly
   (`busy=0`, WAL truncated to 0) once released — the intended, clean
   result. **Any future fixture/test harness that mixes one-shot `sqlite3`
   invocations with persistent-connection assumptions will hit this same
   confound** — worth flagging for #21 (fixture corpus for WAL/hot-journal
   states), which will need a similar persistent-session or fifo-blocking
   approach (as 004_wal_reading already found) whenever a test needs the
   WAL to stay non-empty across steps.

2. **The WAL reader-mark protocol is genuinely, bidirectionally
   interoperable at the raw `fcntl` level — confirmed by a real stock
   `sqlite3` checkpointer backing off, not just by re-simulating its own
   lock check.** Once experiment 4's harness confound (finding 1) was
   fixed, holding `WAL_READ_LOCK(2)` (byte 125) in SHARED mode with
   `aReadMark[2]` set to our claimed frame correctly made a live
   `PRAGMA wal_checkpoint(TRUNCATE)` from a separate `sqlite3` process
   report `busy=1` and leave the WAL file untouched (still 20632 bytes);
   releasing the lock immediately let the same pragma succeed (`busy=0`,
   WAL truncated to 0). This is exactly the byte-for-byte, header-layout-
   accurate mechanism the issue's falsification criteria worried might have
   "undocumented behavior" — it did not, once the mxFrame/aReadMark offsets
   were taken from the real source rather than assumed.

3. **Experiment 3's design necessarily uses a fixed sleep, not a sentinel
   sync, because the event under test (`sqlite3` blocked retrying an
   EXCLUSIVE lock upgrade) has no observable side effect until it either
   succeeds or times out.** `send_and_sync` (a `SELECT` sentinel read back
   over the session's stdout) works for every other synchronization point
   in this spike because those statements complete immediately. A `COMMIT`
   that's stuck in SQLite's internal busy-retry loop for
   `PRAGMA busy_timeout` cannot be sentinel-synced past — there is nothing
   to read until it's done retrying. The 200ms fixed sleep before probing
   is a real timing assumption (long enough to guarantee sqlite3 has taken
   `PENDING_BYTE` and started its exclusive-range retry loop, well short of
   the 3000ms `busy_timeout` given so the probe always lands mid-retry).
   Fine for a throwaway spike exercised interactively; would need a more
   robust signal (e.g. polling `lsof`/`fcntl(F_GETLK)` from the harness
   itself for the PENDING byte's holder) if this pattern were ever promoted
   into permanent test infrastructure.

4. **The POSIX close()-drops-all-locks trap is real and exactly as
   documented, and the fix is "never call close() on a second logical
   reference," not "use `dup()`."** Confirmed empirically: locking byte X
   via `fd_a`, then opening and closing an entirely unrelated `fd_b` to the
   *same inode in the same process*, silently drops the lock taken via
   `fd_a` — a genuine external probe process could then acquire it
   uncontested. Worth noting for anyone tempted to reach for `dup()` as the
   fix: `dup()` does **not** help, since POSIX fcntl record locks are
   scoped to (process, inode), not to the open file description — closing
   *any* fd for that inode from that process drops the process's locks on
   it, dup'd or not. The only fix demonstrated here (and the shape of
   SQLite's real `unixInodeInfo` fd cache) is to never perform the second
   `close(2)` at all: reuse the existing fd for a second logical need and
   only truly close once every logical reference is done. A real
   implementation needs a per-inode (device+inode) refcounted cache, not a
   one-off `dup()`.

## Consequences for plan.md's Concurrency Contract

- Tier 0 / V1's "safe reader" obligation (SHARED-lock-correctly, hot-journal
  and busy detection) is **validated**: experiments 1–3 show our raw
  `fcntl` locks interop correctly with a live stock writer/reader in both
  directions for the journal-mode lock ladder, including the anti-starvation
  PENDING semantics.
- V6's "exact `-shm` layout and lock-slot protocol; live interop... as
  acceptance test" is **validated** for the reader-mark mechanism
  specifically (experiment 4) — the byte-for-byte header layout from
  wal.c's `WalIndexHdr`/`WalCkptInfo` is correct and sufficient.
- V3's fd-cache workaround for the close() trap is **validated in shape**
  (experiment 5): the fix is a per-inode refcounted fd cache, not `dup()`.
  A real implementation will need the cache keyed by (device, inode), not
  just a single global fd — untested here since this spike only opens one
  file at a time.
- **Open item, not closed by this spike:** Linux was not exercised (macOS
  sandbox only). POSIX `fcntl` semantics are the same standard on both, but
  this spike does not independently confirm Linux behavior — worth a quick
  follow-up run in CI (a Linux runner) before fully closing the "macOS and
  Linux both exercised" exit criterion.
- No falsification occurred: none of "lock byte offsets/semantics don't
  interop," "sqlite3 ignores our locks or vice versa," "the `-shm` slot
  protocol has undocumented behavior," or "the fd-cache workaround is
  insufficient" were observed. The one real surprise (finding 1) was a
  harness/methodology issue, not a locking-protocol issue — once corrected,
  the underlying hypothesis held cleanly.

## Token spend

Budget: issue's own 2–3 day timebox, treated as the effort budget (issue
lacked a `## Complexity` section — noted and confirmed with the user before
starting). Spend: roughly matched the estimate for a single-agent direct-mode
spike; no `Workflow`/multi-agent fan-out was used or needed.
