//! Tier 0 — READ CORE, the never-droppable contract (spec 001-architecture
//! Requirement 4, `plan.md` Core Definition). Every test here must be
//! green from day one, forever: this file must never carry `#[ignore]`
//! (see `tools/assurance.py`'s `tier_model()` and CLAUDE.md's tier-stub
//! convention).
//!
//! Distinct from `tests/corpus/`: the corpus harness is
//! evidence-oriented ("do we match the oracle on these fixtures?"); tier
//! tests are claim-oriented ("does the system uphold contract clause
//! N?"), named after the promise they discharge rather than the fixture
//! they happen to use. Where a claim is best checked by diffing against
//! the pinned oracle, this file reuses `tests/corpus`'s own `oracle.rs`
//! and `harness.rs` verbatim via `#[path]` rather than duplicating their
//! logic.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

#[path = "../corpus/harness.rs"]
#[allow(dead_code)]
mod harness;
#[path = "../corpus/oracle.rs"]
#[allow(dead_code)]
mod oracle;

use harness::{discover_fixtures, FAMILIES};
use oracle::{oracle_csv_output, oracle_list_output, pinned_oracle, skip_no_oracle};
use sqlite_rs::dump::dump_database;
use sqlite_rs::format::{format_csv_value, format_list_value};
use sqlite_rs::record::Value;
use sqlite_rs::vfs::{companion_path, UnixVfs};
use std::path::Path;

fn journalstates_fixture(name: &str) -> std::path::PathBuf {
    oracle::corpus_dir().join("journalstates").join(name)
}

/// Copies a fixture (and its `-wal`/`-journal` companion, if present) into
/// an isolated temp directory, removed on drop.
///
/// Needed because tests here run concurrently within one binary, and some
/// paths open a WAL fixture's real, adjacent `-shm` file: `dump_database`
/// via `vfs::claim_wal_read_lock` (read-only, but writes reader-mark bytes
/// into an existing `-shm`), and — more importantly — the pinned oracle
/// itself when `oracle_list_output`/`oracle_csv_output` shell out to a
/// real `sqlite3` against a WAL db, which creates its own `-shm` file on
/// connect. Two tests hitting the same shared, committed fixture path at
/// once can have one see the other's `-shm` mid-creation and read it as
/// too short. Fixtures without an adjacent `-wal`/`-journal`
/// (most families) don't need this, but applying it uniformly is cheap
/// and removes the whole race class rather than special-casing WAL
/// fixtures only.
struct IsolatedFixture {
    dir: std::path::PathBuf,
    path: std::path::PathBuf,
}

impl IsolatedFixture {
    fn new(path: &Path) -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "sqlite-rs-tier0-isolated-{}-{n}-{}",
            std::process::id(),
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("fixture"),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join(path.file_name().unwrap());
        std::fs::copy(path, &dest).unwrap();
        for suffix in ["-wal", "-journal"] {
            let companion = companion_path(path, suffix);
            if companion.exists() {
                std::fs::copy(&companion, companion_path(&dest, suffix)).unwrap();
            }
        }
        Self { dir, path: dest }
    }
}

impl Drop for IsolatedFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn dump_t_rows(path: &Path) -> Vec<(i64, String)> {
    let result =
        dump_database(&UnixVfs, path).unwrap_or_else(|e| panic!("dumping {}: {e}", path.display()));
    let t = result
        .tables
        .iter()
        .find(|t| t.name == "t")
        .unwrap_or_else(|| panic!("table t not found in {}", path.display()));
    t.rows
        .iter()
        .map(|row| {
            let a = match &row[0] {
                Value::Integer(i) => *i,
                other => panic!("expected integer column a, got {other:?}"),
            };
            let b = match &row[1] {
                Value::Text(s) => s.to_string(),
                other => panic!("expected text column b, got {other:?}"),
            };
            (a, b)
        })
        .collect()
}

/// "Any feature-bearing file dumps all rows" — every non-`invalid`
/// fixture family, byte-for-byte against the pinned oracle's `-list` and
/// `-csv` renderings. Skips (not fails) when no pinned oracle is present,
/// matching `tests/corpus/dump_oracle_test.rs`'s convention.
#[test]
fn t0_any_feature_bearing_file_dumps_all_rows() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("t0_any_feature_bearing_file_dumps_all_rows");
        return;
    };

    let mut checked_tables = 0usize;
    for family in FAMILIES {
        if *family == "invalid" {
            continue;
        }
        for path in discover_fixtures().into_iter().filter(|p| {
            p.parent()
                .and_then(|d| d.file_name())
                .and_then(|n| n.to_str())
                == Some(*family)
        }) {
            if path.file_name().and_then(|n| n.to_str()) == Some("hot_journal.db") {
                continue; // covered by t0_hot_journal_recovers_committed_state below
            }
            // Isolated: the oracle shell-out below creates its own `-shm`
            // when connecting to a WAL fixture, which would otherwise race
            // other tests reading the same shared, committed path.
            let isolated = IsolatedFixture::new(&path);
            let path = &isolated.path;
            let result = dump_database(&UnixVfs, path)
                .unwrap_or_else(|e| panic!("dumping {}: {e}", path.display()));

            for table in &result.tables {
                if table.columns.is_empty() {
                    continue; // no parsed column names to build a SELECT list from
                }
                let mine_list: Vec<String> = table
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(format_list_value)
                            .collect::<Vec<_>>()
                            .join("|")
                    })
                    .collect();
                let mine_list = mine_list.join("\n") + if mine_list.is_empty() { "" } else { "\n" };
                let oracle_list = oracle_list_output(&oracle, path, &table.name, &table.columns);
                assert_eq!(
                    mine_list,
                    oracle_list,
                    "-list mismatch: {} / table {}",
                    path.display(),
                    table.name
                );

                let mine_csv: Vec<String> = table
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(format_csv_value)
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .collect();
                let mine_csv = mine_csv
                    .iter()
                    .map(|line| format!("{line}\r\n"))
                    .collect::<String>();
                let oracle_csv = oracle_csv_output(&oracle, path, &table.name, &table.columns);
                assert_eq!(
                    mine_csv,
                    oracle_csv,
                    "-csv mismatch: {} / table {}",
                    path.display(),
                    table.name
                );

                checked_tables += 1;
            }
        }
    }
    assert!(
        checked_tables > 0,
        "no tables were checked — bug in this test"
    );
}

/// "Invalid input never panics" — every `invalid` family fixture must
/// come back as a graceful `Err`, never a panic or an `Ok` misread.
#[test]
fn t0_invalid_input_never_panics() {
    let mut checked = 0usize;
    for path in discover_fixtures().into_iter().filter(|p| {
        p.parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            == Some("invalid")
    }) {
        let result = dump_database(&UnixVfs, &path);
        assert!(
            result.is_err(),
            "{} should have failed to dump, not decoded successfully",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no invalid fixtures were checked — bug in this test"
    );
}

/// A hot rollback journal must never be silently ignored in favour of the
/// main file's uncommitted, spilled pages — V1's refuse-and-explain was
/// upgraded by #172 to actual recovery, so the committed-before state
/// (not an error) is what a reader now sees.
///
/// Copies the fixture pair into a scratch temp dir first: recovery
/// mutates the main file and deletes the journal in place, and the
/// checked-in fixture under `tests/corpus/fixtures/` must stay
/// byte-identical for every other test that reads it.
#[test]
fn t0_hot_journal_recovers_committed_state() {
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-tier0-hot-journal-recovery-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("hot_journal.db");
    std::fs::copy(journalstates_fixture("hot_journal.db"), &db_path).unwrap();
    std::fs::copy(
        journalstates_fixture("hot_journal.db-journal"),
        dir.join("hot_journal.db-journal"),
    )
    .unwrap();

    assert_eq!(
        dump_t_rows(&db_path),
        vec![(1, "committed-before".to_string())]
    );
    assert!(!dir.join("hot_journal.db-journal").exists());

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Uncheckpointed WAL frames must be visible, merged over the main
/// file's stale page — across every WAL-fixture variant (plain,
/// big-endian checksum, foreign-frame rejection, rolled-back trailer).
#[test]
fn t0_wal_pending_rows_visible() {
    let dump_isolated = |name: &str| {
        let isolated = IsolatedFixture::new(&journalstates_fixture(name));
        dump_t_rows(&isolated.path)
    };

    assert_eq!(
        dump_isolated("wal_pending.db"),
        vec![
            (1, "one".to_string()),
            (2, "two".to_string()),
            (3, "three".to_string()),
        ]
    );
    assert_eq!(
        dump_isolated("wal_pending_bigendian.db"),
        dump_isolated("wal_pending.db")
    );
    let stale_rows = dump_isolated("wal_pending_stale.db");
    assert_eq!(
        stale_rows,
        vec![(10, "ten".to_string()), (11, "eleven".to_string())]
    );
    assert!(!stale_rows.iter().any(|(_, b)| b.contains("STALE-FRAME")));
    assert_eq!(
        dump_isolated("wal_pending_trailing.db"),
        vec![(1, "committed-before".to_string())]
    );
}

/// UTF-16 (both endiannesses) must decode to the same text as the UTF-8
/// fixture holding identical content, per header byte 56.
#[test]
fn t0_utf16_decodes_like_utf8() {
    let dump = |name: &str| {
        let path = oracle::corpus_dir().join("encodings").join(name);
        dump_database(&UnixVfs, &path).unwrap_or_else(|e| panic!("dumping {name}: {e}"))
    };
    let utf8 = dump("utf8.db");
    let utf16le = dump("utf16le.db");
    let utf16be = dump("utf16be.db");

    assert!(!utf8.tables.is_empty(), "utf8.db has no tables");
    for (le, be) in utf16le.tables.iter().zip(utf16be.tables.iter()) {
        assert_eq!(
            le.rows, be.rows,
            "utf16le/utf16be mismatch in table {}",
            le.name
        );
    }
    for (utf8_table, le_table) in utf8.tables.iter().zip(utf16le.tables.iter()) {
        assert_eq!(
            utf8_table.rows, le_table.rows,
            "utf8/utf16le mismatch in table {}",
            utf8_table.name
        );
    }
}

/// Every page-size and reserved-bytes boundary fixture must open and
/// decode without error.
#[test]
fn t0_all_page_sizes_and_reserved_bytes_readable() {
    for name in [
        "page_size_512.db",
        "page_size_65536.db",
        "reserved_bytes_0.db",
        "reserved_bytes_12.db",
    ] {
        let path = oracle::corpus_dir().join("pagesizes").join(name);
        dump_database(&UnixVfs, &path).unwrap_or_else(|e| panic!("dumping {name}: {e}"));
    }
}

/// Feature-bearing files (FTS5, R-Tree, autovacuum, STRICT+generated
/// columns) must remain raw-row readable even though this crate has no
/// query-level support for the extensions themselves — virtual tables
/// degrade to a warning, not a hard failure.
#[test]
fn t0_feature_bearing_files_are_raw_row_readable() {
    for name in [
        "autovacuum.db",
        "fts5.db",
        "rtree.db",
        "strict_generated.db",
    ] {
        let path = oracle::corpus_dir().join("features").join(name);
        dump_database(&UnixVfs, &path).unwrap_or_else(|e| panic!("dumping {name}: {e}"));
    }
}
