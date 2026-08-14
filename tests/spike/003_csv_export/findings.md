# Spike findings — #12 / 003_csv_export

Throwaway single-`main.rs` experiment (see `src/main.rs`, `gen_fixture.sh`).
Run with `make run` (or `cargo run -- <path.db>`) inside this directory;
regenerate `fixture.db` with `./gen_fixture.sh` (requires `sqlite3` on
`PATH`). Renumbered from the issue's own "005" to **003** — the next free
`tests/spike/` slot after `001_parser`/`002_file_reading` (see #15's
discarded spike, which briefly held 003 and freed it back up).

Unlike spike 002 (which predated `src/header.rs`/`src/record/`/`src/vfs/`
and reimplemented everything by hand), this spike depends on the real
crate (`sqlite-rs = { path = "../../.." }`) and reuses `DatabaseHeader`,
`Vfs`/`VfsFile`, `record::decode_record`/`decode_varint` — it only
prototypes what doesn't exist yet: interior-node traversal, overflow-chain
reassembly, multi-table `sqlite_master` enumeration, and a minimal DDL
column extractor.

## Hypothesis: SURVIVES, with two real gotchas found

Interior-node traversal (1500-row `bulk` table, forcing table-b-tree
interior pages) and overflow-chain reassembly (a 60000-byte blob spanning
~15 overflow pages in `big`) both **match the `sqlite3` oracle exactly**
on the first real attempt — the two things spike 002 explicitly left
unvalidated. Multi-table enumeration via `sqlite_master` also worked
cleanly. But two behaviors *not* anticipated by #32/#33/#34's ticket text
were found — see findings 1 and 3.

## Exit criteria

- [x] All tables of `fixture.db` exported, `sqlite3 -csv`/oracle parity per table (bulk: byte-identical; typed: values match, see finding 2 for known display-format divergence; big: blob byte-identical via `quote()`)
- [x] Multi-page traversal + overflow exercised (`bulk` = 1500 rows across interior+leaf pages; `big` = 60000-byte blob, ~15-page overflow chain)
- [x] CSV rendering decisions documented (NULL, quoting, blobs, floats) — see findings 2 and 4
- [x] Findings written (this file) + `make spike-003`

## Findings

1. **The rowid-alias optimization is a real, silent-corruption trap — not just a b-tree-walking detail.** A column declared exactly `INTEGER PRIMARY KEY` (single column, not composite, not `WITHOUT ROWID`) is **not stored in the record at all** — SQLite encodes it as serial type 0 (NULL) and expects the reader to substitute the cell's own rowid for that column. First attempt at exporting `bulk` (`id INTEGER PRIMARY KEY, val TEXT`) produced an empty `id` column for every row — decoding the record faithfully is not enough; the DDL must be inspected for this pattern (naive detection: a column def containing `INTEGER` and `PRIMARY KEY` as whole words, case-insensitive) and the cursor layer must special-case it. Confirmed correct after the fix: `bulk_fixture.csv` diffs byte-identical against `sqlite3 -csv "SELECT id, val FROM bulk ORDER BY id"`. **This belongs in #32's or #34's acceptance criteria explicitly** — it's not covered by either ticket's text today, and it's easy to ship a cursor that "works" on every fixture except tables with an integer-primary-key alias (which is an extremely common real-world schema pattern, likely more common than not).
2. **Float display formatting still doesn't match `sqlite3`'s, confirming spike 002 finding 3 — not a new issue, just re-confirmed on a fresh path.** `2.5e300` still round-trips through Rust's `f64::to_string()` as a ~300-digit decimal instead of `sqlite3`'s `2.5e+300`. Not re-verified bit-identical this time (already proven in spike 002); flagging again only because it's directly visible in `typed_fixture.csv`. Reinforces spike 002's recommendation: this is step 9's (output contract) decision to make, not step 4's bug to fix.
3. **`sqlite3 -csv`'s quoting rule is not standard RFC4180 (comma/quote/newline) — it also quotes on any embedded space, and on a *leading or trailing* single-quote character specifically (but not mid-string).** Empirically probed (not from source): `'a b'`, `' ab'`, `'ab '` all get quoted (any space, anywhere); `"ends_with_quote'"` and `"'starts_with_quote"` get quoted, but `"mid'quote"` and `"a'b"` (apostrophe only in the middle) do not. This means my simple comma/quote/newline escaper in `render_value`/`csv_escape` will **not** byte-match `sqlite3 -csv` output for values containing spaces or boundary apostrophes (e.g. `quote()`'s own `X'DEADBEEF'` blob rendering — ironic, since that's exactly the string this spike renders for blobs). Values without those (the `bulk`/`big` fixtures) were unaffected, which is why those two diffed identical while `typed`'s blob column would not, byte-for-byte, without adopting sqlite3's exact heuristic. **Decision needed at step 9**: either replicate this idiosyncratic heuristic for true CSV oracle-parity, adopt standard RFC4180 quoting and accept the documented divergence, or (recommended) avoid CSV as the diffable format for oracle tests entirely and use a pipe-delimited dump instead, exactly as spike 002 and this project's actual corpus-harness likely will — CSV's quoting ambiguity is a self-inflicted complication for a byte-diff oracle, not a inherent requirement.
4. **Blob-as-`X'HEX'` (matching `quote()`'s own format) is the right rendering choice for oracle-diffability, confirmed working end-to-end** — including through a full 60000-byte overflow chain. Recommend step 9 adopt this convention (or at minimum use it for any diff-based test that has to go through `sqlite3 -csv`, per finding 3's caveat above).
5. **A real-world virtual-table interaction surfaced a genuine step 4/5 boundary, exercised against the actual `tests/corpus/fixtures/features/fts5.db` fixture (not part of #12's own described fixture, added as a bonus check).** FTS5's shadow tables are ordinary `sqlite_master` entries (`type='table'`, not virtual) — the virtual-table skip logic correctly skipped the FTS5 entry itself (`CREATE VIRTUAL TABLE t USING fts5(txt)`), but then this spike's code (table-b-tree-only, as scoped) tried to walk `t_idx` as a table b-tree and **panicked** on encountering page type `0x0a` (index leaf). Checked `t_idx`'s and `t_config`'s actual DDL: both are declared `... WITHOUT ROWID` explicitly. **This confirms, on a real fixture rather than by inference, that #34's own note — "WITHOUT ROWID / STRICT markers detected (feeds step 5 cursor selection)" — is a hard, load-bearing dependency, not a nice-to-have**: there is no way to know a `sqlite_master` table entry needs the index-b-tree cursor (#33) instead of the table-b-tree cursor (#32) without reading its DDL for `WITHOUT ROWID` first. A "minimal DDL reader" that skips this marker will silently misroute (or, as here, loudly crash) on any schema using FTS5, or any hand-written `WITHOUT ROWID` table. The panic itself is *correct* behavior for this spike's declared scope (index b-trees are explicitly out of scope, "loud asserts" is the stated fallback) — the finding is that #32 (step 4) cannot safely attempt "read every table" alone; #34 (step 7)'s WITHOUT ROWID detection is a prerequisite gate, not a downstream enhancement, for anything touching real-world virtual-table shadow schemas.

## Conclusion

The two things spike 002 left completely unvalidated — interior-node
traversal and overflow-chain reassembly — both worked correctly against
real multi-page/overflow data on the first real attempt, using the
formulas transcribed from SQLite's documented file format (no rustc/tool
surprises here, unlike #15's spike). That's a strong signal for #32.

But this spike earns its keep by finding two things the ticket text
*didn't* anticipate: the rowid-alias optimization (finding 1, a real
correctness gap that has to land in #32 or #34, not later) and the
WITHOUT-ROWID-detection-as-hard-dependency between #34 and #33 (finding
5, confirmed on a real fixture, not just inferred from the ticket
cross-references). Findings 2-4 are softer, feeding step 9's still-open
output-contract decisions rather than blocking anything now.

## Go/no-go on #32 / #33 / #34

**GO on #32.** Table-b-tree traversal (interior + leaf) and overflow
reassembly are validated against real multi-page data. Add explicitly to
#32's acceptance criteria: **the rowid-alias case** (a column declared
`INTEGER PRIMARY KEY` is not stored in the record; the cursor/DDL layer
must substitute the cell's own rowid) — this is not optional or an edge
case, it's an extremely common schema pattern and today's ticket text
doesn't mention it at all.

**GO on #33, with a sequencing note.** Nothing here falsifies the index
b-tree approach, but finding 5 shows #33's cursor can't be safely
*selected* without #34's WITHOUT ROWID detection already in place for a
given table. If #33 lands before #34, its own tests should drive cursor
selection directly from fixture metadata (not yet real schema markers) and
treat "wire into DDL-driven selection" as #34's job to close out, not
something #33 can silently skip.

**GO on #34, with a sharpened acceptance criterion.** The "WITHOUT
ROWID / STRICT markers detected (feeds step 5 cursor selection)" line
already in #34's text is correct and now confirmed on a real fixture
(`fts5.db`'s `t_idx`/`t_config`) rather than speculative — recommend
making it a hard MUST with a linked scenario, not a soft note, since
finding 5 shows the failure mode (a crash, not a graceful degrade) when
it's missing.

**Not exercised (deliberately, per #12's own scope):** WAL, locking,
UTF-16, index b-tree key comparison itself, and STRICT-table generated
columns. These remain #33's, spec 003's WAL work, and #34's own problems
respectively — no new information for or against them from this spike.
