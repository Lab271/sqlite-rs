# Spike findings — #4 / 002_file_reading

Throwaway single-`main.rs` experiment (see `src/main.rs`, `gen_fixture.sh`).
Run with `cargo run` inside this directory; regenerate the fixture with
`./gen_fixture.sh` (requires `sqlite3` on `PATH`).

## Hypothesis: SURVIVES

The documented file format (100-byte header → b-tree pages → cells →
records with serial types, per `.openspec/specs/001-architecture/spec.md`)
was sufficient to decode `sqlite_master` and table `t` from a real,
stock-created fixture with **zero undocumented format surprises**. Every
value decoded matches the `sqlite3` oracle exactly (see below). The one
genuine surprise found was in *tooling*, not the *format* — see finding 1.

## Exit criteria

- [x] Dump of `sqlite_master` matches expectation (`type|name|tbl_name|rootpage|sql`, one row for table `t`)
- [x] All 5 value types of table `t` decoded correctly, both rows:
      NULL, INTEGER (positive `42` and negative `-1`), REAL (`3.14` and the
      extreme `2.5e300`), TEXT (ASCII and a non-ASCII string with `é`/`→`),
      BLOB (4-byte and **zero-length**)
- [x] Written findings (this file)
- [x] Go/no-go on the 9-ticket V1 breakdown (issue #5) — **GO**, with scope notes below

Verification method: `cargo run` output eyeballed against
`sqlite3 fixture.db -mode list -separator '|' -nullvalue NULL "SELECT a,b,c,quote(d),e FROM t;"`
(the `.dump` diff the issue suggests) — `quote()` was used on the blob
column specifically, since raw blob bytes aren't printable and would
otherwise produce misleading garbled output rather than a real mismatch.

```
oracle:  42|hello|3.14|X'DEADBEEF'|NULL
mine:    a=42 b=hello c=3.14 d=X'DEADBEEF' e=NULL

oracle:  -1|unicode: héllo→|2.5e+300|X''|7
mine:    a=-1 b=unicode: héllo→ c=2.5e300 d=X'' e=7
```

All values match. The `2.5e+300` vs `2.5e300` difference is display-only —
confirmed bit-identical via `f64::to_bits()` (see finding 2), not a decode bug.

## Findings

1. **The fixture's `reserved_space` header byte is 12, not 0 — because of the `sqlite3` binary used, not the format.** macOS's system `/usr/bin/sqlite3` (3.51.0) is compiled with `CODEC=see-cccrypt` / `HAS_CODEC_RESTRICTED` (SQLite Encryption Extension via CommonCrypto) — this reserves 12 bytes per page for the codec even when no encryption key is set. A fixture generated with a vanilla `sqlite3` build (the amalgamation from sqlite.org) would very likely have `reserved_space=0`. This isn't a bug — the header format handled it fine, and the code reads `reserved_space` from the header rather than assuming 0 — but it's a real gotcha for step 8 (fixture + oracle harness): **fixture generation should either pin a vanilla, non-codec `sqlite3` build, or deliberately test both `reserved_space=0` and `reserved_space>0` fixtures**, since right now it's accidental which one you get depending on whose machine/CI runner produces the fixture. Worth a note in step 8's ticket.
2. **Page 1's cell pointer array is relative to the start of the page (byte 0), not to byte 100 where its b-tree header begins.** Page 1 is the only page with the 100-byte file header prepended before its b-tree page header — easy to misread as "offsets are relative to where the b-tree header starts." They aren't; cell pointers on every page (including page 1) are page-relative from byte 0. Confirmed by construction (the code adds `page_start`, not `header_start`, to resolve pointers) and correct output. Worth flagging explicitly in step 1 or step 4's ticket description since it's a one-line detail that silently produces wrong offsets if missed.
3. **Float display formatting differs from `sqlite3`'s `quote()`/CLI output, but the decoded bits are correct.** Rust's default `{}` `Display` for `f64` spells `2.5e300` out as a ~300-digit decimal string rather than switching to scientific notation; `sqlite3` uses something closer to `%g`. Confirmed the underlying value is bit-identical via `f64::to_bits()` (`7e4ddd4baa009303` either way) — this is a presentation concern for a future `dump` CLI (step 9), not a record-decoder bug (step 3). The real decoder shouldn't need to reproduce `sqlite3`'s exact float formatting algorithm unless byte-identical `dump` output (the V1 acceptance gate) is interpreted to include exact string formatting, not just correct values — worth clarifying in step 9's ticket.
4. **Not exercised (deliberately out of scope for this atomic experiment):** overflow chains (this fixture's rows all fit locally), multi-page / interior table b-trees (both tables here fit on one leaf page), index b-trees, WAL frames, and DDL parsing beyond printing the raw `sql` text column verbatim. The code asserts loudly (panics) rather than silently mishandling these if a future fixture happens to hit them — see the `assert!`/`panic!` calls in `read_table_leaf` and `decode_serial_value`. These map directly to steps 4 (overflow + multi-page), 5 (index b-trees), 6 (WAL), and 7 (DDL reader) and were always meant to be separate, larger tickets, not gaps in this spike.

## Conclusion

The Tier 0 READ CORE architecture in `001-architecture/spec.md` needed
**zero revision** — every byte-level detail this spike touched (header
layout, page-relative cell pointers even on page 1, varint/serial-type
encoding) matched on the first real attempt against a real file. That's a
stronger result than "the hypothesis survives": it means the spec is
trustworthy enough to implement straight from, without another round of
format archaeology, when issue #5's step tickets get written.

Findings 1-3 above aren't new standalone issues to file — they're concrete
acceptance-criteria and gotchas that belong directly in the step tickets
they map to (steps 1, 4, 8, 9) when issue #5 specs each one, per its own
"tickets created and specced one at a time" note. Finding 1 (the
codec-enabled system `sqlite3`) is the one to resolve *first*, though,
ahead of any other work: it affects every fixture anyone generates for this
project going forward, not just this spike's.

Zooming out past this single spike: together with issue #1's parser-toolchain
spike, this closes out both halves of `plan.md`'s "Parallel Tracks" —
Frontend (Tokenizer → Parser → AST, now pointed at `pomelo`) and Storage
(VFS → Pager → B-Tree, now validated against a real file with no
architectural surprises). Neither track has a foundational unknown left
blocking it from starting real ticket work.

## Go/no-go on the issue #5 9-step breakdown

**GO.** Nothing here falsifies the breakdown or the Tier 0 architecture spec.
Per-step notes for whoever picks up issue #5:

- **Step 1** (VFS + header): validated for `page_size=4096`, no auto-vacuum. Recommend the real ticket's fixture corpus covers more page sizes (512, 65536) and an auto-vacuum database, since those header fields were read but not exercised here.
- **Step 2** (Pager): trivial page-fetch-by-number logic worked as expected, including honoring a nonzero `reserved_space` (found by accident — see finding 1). No hot-journal or cache-eviction scenarios touched (correctly out of scope for this spike).
- **Step 3** (Record decoder): high confidence — every serial type this spike could reasonably hit (0, 1, 7, text, blob including the zero-length blob edge case) round-tripped correctly on the first real attempt against a real file, not just hand-constructed bytes.
- **Step 4** (Table b-tree cursor): the base leaf-page walk is solid; overflow chains and multi-page (interior) b-trees are the two things this spike did *not* prove and should be the first things the real ticket's fixture corpus forces (e.g. one row large enough to overflow, one table with enough rows to span 2+ pages).
- **Step 8** (Fixture + oracle harness): start here as planned, but bake in finding 1 (pin a non-codec `sqlite3`, or explicitly test both reserved-byte cases) and reuse the `quote()`-on-blob-columns oracle-query trick from this spike's verification method — plain `SELECT *` output is not diffable for blob columns.
- **Steps 5, 6, 7, 9**: untouched by this spike, as expected — no new information for or against them.
