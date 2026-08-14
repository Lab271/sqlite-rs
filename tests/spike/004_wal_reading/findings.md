# Spike findings — #7 / 004_wal_reading

Throwaway single-crate experiment (see `src/main.rs`, `src/wal.rs`,
`gen_fixture.sh`), following spike 002's (#4) precedent of a disposable,
self-contained crate rather than depending on `src/` — spike 002's own
b-tree walk and record decoder are copied in, not imported. Run with
`cargo run` (or `make run`) inside this directory; regenerate all four
fixture pairs with `./gen_fixture.sh` (requires `sqlite3` and `python3` on
`PATH`; the big-endian variant needs `python3` specifically since this
host's `sqlite3` can't produce that checksum mode itself — see finding 2).

## Hypothesis: SURVIVES

The documented WAL format — 32-byte header, 24-byte frame headers, the
two-word rolling checksum, salt-based generation tagging, commit-frame
detection via a non-zero db-size field — is sufficient to reconstruct the
correct page view of a WAL-mode database with uncheckpointed frames,
matching `sqlite3`, with no `-shm` file involved. One real surprise
(finding 2, byte-order mapping) and one workflow gotcha (finding 1,
fixture generation) — both in tooling/format-detail, not in the
architecture.

## Exit criteria

- [x] Uncheckpointed rows visible, matching oracle — `fixture.db` +
      `fixture.db-wal`: 3 rows across 3 separate commits to the same page,
      all invisible if you read `fixture.db` alone (it still has the
      table's *empty* leaf page — see finding 1), all visible once the
      WAL is merged in. `sqlite3`, run against a scratch copy, agrees
      exactly (`1|one`, `2|two`, `3|three`).
- [x] Both checksum-endianness paths exercised — `fixture.db-wal` (native,
      magic `0x377f0682`, what this host's `sqlite3` actually writes) and
      `fixture_bigendian.db-wal` (a synthetic magic-`0x377f0683` variant,
      re-checksummed independently in Python — see finding 2). Both decode
      to the identical 3 rows in this repo's Rust reader, **and** the real
      `sqlite3` binary independently accepts the synthetic big-endian file
      and returns the same 3 rows — two independent confirmations, not
      just the Rust code agreeing with itself.
- [x] Stale/uncommitted frames correctly ignored — two distinct scenarios,
      both organically produced by real SQLite (no byte-forgery):
      - `fixture_trailing.db-wal`: a transaction with `cache_spill=1` and
        a 1 KB cache forced 4 dirty pages to spill into the WAL as
        non-commit frames *before* the transaction was rolled back. The
        reader (and `sqlite3` itself) correctly shows only the
        previously-committed row, none of the spilled insert.
      - `fixture_stale.db-wal`: one committed frame lifted from an
        entirely unrelated WAL generation (different random salts) is
        appended after this fixture's own last commit. The reader stops
        the instant it hits the salt mismatch and correctly shows only
        this generation's 2 rows — the poisoned `'STALE-FRAME-MUST-NOT-APPEAR'`
        row never surfaces. `sqlite3` agrees.
- [x] Findings written up (this file); step 6 spec notes below.

## Findings

1. **Closing the last connection to a WAL-mode database fully checkpoints it — even with `PRAGMA wal_autocheckpoint=0`.** That pragma only disables the *size-triggered* automatic checkpoint; SQLite still performs a full checkpoint when the last connection closes, truncating the WAL back to empty. This makes "just write some rows with `sqlite3 db.file "INSERT ..."`, then copy `db.file` + `db.file-wal`" **not work** for building an uncheckpointed-WAL fixture — by the time the CLI process exits and you can copy the files, the WAL is already empty. The fix (used throughout `gen_fixture.sh`): open a second connection that starts a read transaction *before* any of the writes happen and holds it open (blocked reading from a `mkfifo`) — while that reader is alive, the writer's close-time checkpoint is blocked, so the WAL keeps its committed frames long enough to `cp` both files. This is exactly the mechanism issue #21 (fixture corpus for WAL/hot-journal states) will need — worth linking this spike from there when #21 is picked up, since #21 was still unstarted with no tooling built when this spike began.
2. **The WAL magic number's checksum-mode meaning is the reverse of what a first reading suggests — verified against a real, `sqlite3`-produced file, not assumed from memory.** Magic `0x377f0682` is the common case that *this host's* `sqlite3` actually writes, and it does **not** mean "big-endian checksums" — it means "native byte order," i.e. whatever the byte order of the machine that wrote the WAL happens to be (little-endian here). Magic `0x377f0683` is the less common, portable mode that always forces big-endian regardless of host. Confirmed two ways: (a) manually computing the WAL header's own checksum by hand in Python against the raw bytes of a real fixture only matched the stored value using little-endian (native, on this host) word reads, not big-endian; (b) a synthetic `fixture_bigendian.db-wal` — same content, magic flipped to `0x83`, every checksum independently recomputed in Python using explicit big-endian arithmetic — was accepted by both this repo's Rust reader *and* the real `sqlite3` binary, producing identical rows either way. This detail is genuinely easy to get backwards from the name alone (native is the *default*, not the *exception*) and is worth flagging explicitly in step 6's ticket, since a reader that gets this backwards will silently pass on the common case (most real-world WAL files use the default native mode, so a swapped mapping still "looks" wrong immediately — but it's worth stating the correct direction up front rather than re-discovering it) and only fail on the rarer big-endian-forced files.
3. **Page 1's cell-pointer-array quirk from spike 002 (cell pointers are relative to the page's own start, byte 0, even on page 1, never to where the 100-byte header ends the b-tree header begins) applies unchanged here** — this spike's `read_table_leaf` is a direct copy of spike 002's, and needed no adjustment for the WAL overlay beyond swapping "read bytes from `self.bytes` at a page offset" for "read bytes from `read_page(page_num)`, which may come from the WAL." The b-tree/record layer is entirely agnostic to where a page's bytes came from — a clean seam for the real implementation (a `Pager` that resolves page N from either the WAL or the main file should be fully transparent to everything above it).
4. **Forcing a genuine (not byte-forged) uncommitted/spilled frame requires `PRAGMA cache_spill=1` *and* a very small `cache_size` expressed as negative KB (e.g. `-1`), not a small positive page count.** `PRAGMA cache_size=2` (2 pages) did **not** force a spill even across an 8000-row, 10-page transaction — SQLite's pager apparently tolerates a small positive page-count cache_size without spilling as aggressively as a KB-based negative value does. This is a fixture-generation gotcha worth remembering for #21 or any future test that needs a genuinely in-flight, uncommitted WAL write rather than a synthetic one.
5. **Merely opening a fresh `sqlite3` connection to read a WAL-mode fixture checkpoints it away on that connection's close** — running the "oracle" diff (`sqlite3 fixture.db "SELECT * FROM t;"`) directly against the committed fixture files destroys them (confirmed by accident: the first attempt at this spike did exactly that and had to regenerate). Every oracle check in this spike's workflow instead copies the fixture pair to a scratch location first and reads the copy. Worth calling out for step 8's fixture + oracle harness (#10): any tooling that diffs a WAL fixture against `sqlite3` for verification must never do so against the fixture that's about to be committed/reused, or it'll self-destruct on first use.
6. **Not exercised (deliberately out of scope, per the issue):** `-shm` file reading, WAL read-locks, live-writer coexistence (spike 004/#8's territory — this repo now has two things numbered "004": this spike's own folder was renamed from `003_wal_reading` to `004_wal_reading` mid-work to avoid a branch collision with a concurrent agent also working issue-adjacent spike numbering, which is *unrelated* to issue #8's own "spike: 004 locking protocol interop" title — worth a note if anyone else picks up numbering here), multi-page/interior b-trees inside the WAL-visible table (this fixture's table always fits on one leaf page), and a WAL spanning more than one checkpoint generation mid-file (this spike's stale-frame test uses a single foreign frame from an entirely separate WAL file, not a real "checkpoint reset then reused region" splice within one physical file — a faithful checkpoint-reuse replay wasn't necessary to validate the salt-rejection mechanism, but it's a narrower test than the full real-world scenario).

## Conclusion

The Tier 0 READ CORE's WAL-reading requirement (`001-architecture` §Requirement 4's "uncheckpointed WAL" scenario) is implementable exactly as documented in SQLite's own file-format reference — **provided the byte-order mapping in finding 2 is taken in the verified direction, not the more "obvious" reading of the magic number's name.** No `-shm` file access was needed for any of the four fixtures (quiescent-WAL reading, matching the issue's own scope). The "latest committed frame per page wins, ignore everything not yet published by a commit" algorithm handled all three families of edge case (multiple commits to the same page, trailing spilled-but-uncommitted frames, and a foreign-generation frame) correctly and identically to real `sqlite3`, without needing any special-casing beyond salt/checksum validation that already had to exist for the happy path.

## Go/no-go on step 6 (issue #5)

**GO.** Concrete notes for step 6's ticket:

- The WAL header (32 bytes) and frame header (24 bytes) layouts, and the two-word rolling checksum algorithm, are exactly as derived in `src/wal.rs` — none of this exists yet in `.openspec/specs/001-architecture/spec.md` (checked: zero mentions of "salt," "checksum," or a frame/WAL byte layout anywhere in `.openspec/specs/`) and should be added as part of step 6's spec, not re-derived from scratch — this spike's `src/wal.rs` doc comments are ready to lift almost verbatim.
- **Finding 2 (checksum byte-order direction) is the one detail step 6 must get right on the first attempt** — it's the "quirkiest corner" the issue predicted, confirmed. Recommend step 6's acceptance criteria explicitly require both a native-mode and a big-endian-mode fixture (this spike's `fixture.db-wal` / `fixture_bigendian.db-wal` are a ready template), not just "checksums validate" against whatever the CI machine's `sqlite3` happens to produce natively.
- The "candidate map, publish only at commit, stop scanning on salt/checksum failure" algorithm in `committed_pages()` is a reasonable reference for the real `Pager`'s WAL-merge logic — it naturally and correctly handles both edge-case families without extra special-casing.
- Step 6's own fixture needs (uncheckpointed WAL, stale/foreign frames, spilled-but-uncommitted frames) overlap heavily with issue #21's still-unstarted WAL/hot-journal fixture corpus work — findings 1 and 4 (the reader-blocking and cache-spill tricks) are directly reusable there, and #21 should link back to this spike's `gen_fixture.sh` rather than re-deriving the same tricks.
- Untouched by this spike, as expected: `-shm` reading, live-writer/locking coexistence (#8's territory), multi-page b-trees under a WAL overlay, and index b-trees — no new information for or against those.
