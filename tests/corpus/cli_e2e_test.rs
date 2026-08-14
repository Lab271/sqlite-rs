//! End-to-end tests of the `sqlite-rs` **binary** (#55), as opposed to
//! `dump_oracle_test.rs`'s library-level diff of the same rendering.
//!
//! The distinction matters: `dump_oracle_test.rs` calls `dump_database`
//! and the `format_*` functions directly, so everything the CLI itself
//! assembles is invisible to it — the CSV **header row**, per-table output
//! filenames, exit codes, and the stdout/stderr split. Those are what a
//! user actually gets, and they are what this file pins.
//!
//! Every fixture is copied to a scratch directory first: `export` writes
//! its `.csv` files as siblings of the source database, and a test must
//! never write into the committed fixture tree.

use crate::harness::{discover_fixtures, FAMILIES};
use crate::oracle::{oracle_csv_with_header_output, pinned_oracle, skip_no_oracle};
use sqlite_rs::dump::dump_database;
use sqlite_rs::vfs::UnixVfs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

/// The binary under test, as built by cargo for this test run — never a
/// `sqlite-rs` that happens to be on `PATH`.
const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-cli-e2e-{label}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Copies `db` plus any `-wal`/`-journal`/`-shm` companion into `dir`,
/// returning the copied database's path. Companions matter: a WAL-mode
/// fixture read without its `-wal` is a different database.
fn copy_fixture(db: &Path, dir: &Path) -> PathBuf {
    let name = db.file_name().unwrap();
    let dest = dir.join(name);
    std::fs::copy(db, &dest).unwrap();
    for suffix in ["-wal", "-journal", "-shm"] {
        let mut companion = db.as_os_str().to_os_string();
        companion.push(suffix);
        let companion = PathBuf::from(companion);
        if companion.exists() {
            let mut dest_companion = dest.as_os_str().to_os_string();
            dest_companion.push(suffix);
            std::fs::copy(&companion, PathBuf::from(dest_companion)).unwrap();
        }
    }
    dest
}

fn run_cli(subcommand: &str, db: &Path) -> Output {
    Command::new(CLI)
        .arg(subcommand)
        .arg(db)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} {subcommand} {}: {e}", db.display()))
}

/// Mirrors `run_export`'s naming: `<sanitized_table>_<stem>.csv`.
fn expected_csv_path(dir: &Path, table: &str, db: &Path) -> PathBuf {
    let stem = db.file_stem().unwrap().to_string_lossy().into_owned();
    let sanitized: String = table
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let sanitized = if sanitized.is_empty() {
        "table".to_string()
    } else {
        sanitized
    };
    dir.join(format!("{sanitized}_{stem}.csv"))
}

/// The header line `export` writes, CRLF-terminated like every other CSV
/// row. Built with the crate's own `csv_quote` on purpose: the point of
/// the header assertions is the *line assembly and terminator*, and
/// per-value quoting parity is already covered against the oracle by
/// `csv_quote_matches_oracle_on_edge_values` below.
fn expected_csv_header(columns: &[String]) -> String {
    let joined = columns
        .iter()
        .map(|c| sqlite_rs::format::csv_quote(c))
        .collect::<Vec<_>>()
        .join(",");
    format!("{joined}\r\n")
}

fn in_family(path: &Path, family: &str) -> bool {
    path.parent()
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        == Some(family)
}

/// The corpus fixtures that are expected to be unreadable, and so are not
/// valid inputs for an oracle diff — the same convention
/// `dump_oracle_test.rs` and `harness_test.rs` use.
fn expected_unreadable(path: &Path) -> bool {
    in_family(path, "invalid")
        || path.file_name().and_then(|n| n.to_str()) == Some("hot_journal.db")
}

/// Every `.csv` the `export` subcommand writes must match
/// `sqlite3 -csv -header` byte for byte — **including the header row**,
/// which the library-level diff in `dump_oracle_test.rs` cannot see at all
/// because it renders from `table.rows` only.
#[test]
fn export_csv_files_match_sqlite3_csv_header_across_corpus() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("export_csv_files_match_sqlite3_csv_header_across_corpus");
        return;
    };

    let mut checked_tables = 0usize;
    let mut skipped_no_columns = 0usize;

    for family in FAMILIES {
        if *family == "invalid" {
            continue;
        }
        for fixture in discover_fixtures()
            .into_iter()
            .filter(|p| in_family(p, family))
        {
            if expected_unreadable(&fixture) {
                continue;
            }

            let dir = scratch_dir("export");
            let db = copy_fixture(&fixture, &dir);

            let output = run_cli("export", &db);
            assert!(
                output.status.success() || !output.stderr.is_empty(),
                "export of {} failed with no diagnostic on stderr",
                fixture.display()
            );

            // The set of tables to expect is the library's own view; this
            // test is about the CLI's *rendering and file layout*, not
            // about rediscovering which tables exist.
            let result = dump_database(&UnixVfs, &db)
                .unwrap_or_else(|e| panic!("dumping {}: {e}", db.display()));

            for table in &result.tables {
                if table.columns.is_empty() {
                    skipped_no_columns += 1;
                    continue;
                }
                let csv_path = expected_csv_path(&dir, &table.name, &db);
                let actual = std::fs::read_to_string(&csv_path).unwrap_or_else(|e| {
                    panic!(
                        "export did not write {} for {} / table {}: {e}",
                        csv_path.display(),
                        fixture.display(),
                        table.name
                    )
                });

                if table.rows.is_empty() {
                    // Deliberate, documented divergence: `sqlite3 -csv
                    // -header` emits *nothing at all* for a zero-row
                    // result — not even the header, since the header is
                    // printed as part of the first row's output. `export`
                    // writes a file per table, and a header-only CSV is
                    // more useful to a consumer than an empty file (which
                    // is indistinguishable from "table missing"), so it
                    // keeps the header. Pinned here so the divergence
                    // stays intentional rather than drifting.
                    let header = expected_csv_header(&table.columns);
                    assert_eq!(
                        actual,
                        header,
                        "empty table should export a header-only CSV: {} / table {}",
                        fixture.display(),
                        table.name
                    );
                    assert_eq!(
                        oracle_csv_with_header_output(&oracle, &db, &table.name, &table.columns),
                        "",
                        "assumption broken: oracle emitted output for a zero-row table \
                         ({} / table {}) — if sqlite3 now prints a header here, drop this \
                         special case and compare directly",
                        fixture.display(),
                        table.name
                    );
                    checked_tables += 1;
                    continue;
                }

                let expected =
                    oracle_csv_with_header_output(&oracle, &db, &table.name, &table.columns);
                assert_eq!(
                    actual,
                    expected,
                    "export CSV mismatch: {} / table {}",
                    fixture.display(),
                    table.name
                );
                checked_tables += 1;
            }

            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    assert!(
        checked_tables > 0,
        "no tables were checked — bug in this test"
    );
    eprintln!(
        "export_csv_files_match_sqlite3_csv_header_across_corpus: \
         {checked_tables} tables checked, {skipped_no_columns} skipped (no parsed columns)"
    );
}

/// A clean, fully readable fixture must exit 0 — the signal a scripted
/// caller uses to conclude the export is complete.
#[test]
fn export_of_a_clean_fixture_exits_zero() {
    let fixture = crate::oracle::corpus_dir().join("btrees/table_single_page.db");
    let dir = scratch_dir("clean-exit");
    let db = copy_fixture(&fixture, &dir);

    let output = run_cli("export", &db);
    assert!(
        output.status.success(),
        "expected exit 0 for a clean fixture, got {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A fixture with a table that cannot be dumped (here: FTS5's virtual
/// table, deliberately skipped with a warning rather than aborting) must
/// still produce output, but must exit non-zero so a scripted caller can
/// detect that the export is *partial*. This is `degraded_exit_code`'s
/// entire purpose and was previously unverified end-to-end.
#[test]
fn export_with_a_skipped_table_exits_nonzero_but_still_writes() {
    let fixture = crate::oracle::corpus_dir().join("features/fts5.db");
    if !fixture.exists() {
        eprintln!("skipping: {} not present", fixture.display());
        return;
    }
    let dir = scratch_dir("degraded");
    let db = copy_fixture(&fixture, &dir);

    let output = run_cli("export", &db);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "expected a non-zero exit when a table is skipped, got 0; stderr: {stderr}"
    );
    assert!(
        stderr.contains("warning:"),
        "expected a warning on stderr explaining the skip; got: {stderr}"
    );
    let written = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "csv"))
        .count();
    assert!(
        written > 0,
        "a partial export must still write the tables it could read"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// An unreadable database must fail cleanly: non-zero exit, a diagnostic
/// on stderr, and nothing on stdout — never a panic (which would surface
/// as a signal-kill exit and an `RUST_BACKTRACE` blob).
#[test]
fn unreadable_database_fails_cleanly() {
    let fixture = crate::oracle::corpus_dir().join("invalid/magic.db");
    if !fixture.exists() {
        eprintln!("skipping: {} not present", fixture.display());
        return;
    }
    let dir = scratch_dir("invalid");
    let db = copy_fixture(&fixture, &dir);

    let output = run_cli("dump", &db);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit 1 for an unreadable database; stderr: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "a failed dump must not emit data on stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !stderr.is_empty(),
        "a failed dump must explain itself on stderr"
    );
    assert!(
        !stderr.contains("panicked at"),
        "a malformed database must produce an error, not a panic: {stderr}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// `dump` puts data on stdout and diagnostics on stderr, so the data
/// stream stays pipeable. Verified on a fixture whose every table is
/// readable, where stdout must carry both the schema line and the rows
/// while stderr stays empty.
#[test]
fn dump_writes_data_to_stdout_and_keeps_stderr_clean() {
    let fixture = crate::oracle::corpus_dir().join("btrees/table_single_page.db");
    let dir = scratch_dir("streams");
    let db = copy_fixture(&fixture, &dir);

    let output = run_cli("dump", &db);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "dump failed: {stderr}");
    assert!(
        stdout.contains("CREATE TABLE"),
        "stdout should carry the schema DDL; got: {stdout}"
    );
    assert!(
        stderr.is_empty(),
        "a clean dump must leave stderr empty; got: {stderr}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The corpus fixtures don't contain values that exercise `sqlite3`'s
/// idiosyncratic CSV quoting heuristic at its edges, which is how two real
/// divergences survived in `csv_quote` — a mid-string `'` was left
/// unquoted, and rows were LF-terminated instead of CRLF. This test builds
/// a database *with the oracle itself* containing exactly those values and
/// diffs the CLI's export against `sqlite3 -csv -header`, so the heuristic
/// is pinned to observed behaviour rather than to a spike's recollection
/// of it.
///
/// Includes a value with an embedded CRLF: SQLite stores TEXT byte-for-byte
/// and applies no line-ending translation, so that value must come back
/// with its own bytes intact (quoted, but not rewritten) — the trap of
/// conflating the CSV row terminator with data content.
///
/// **Deliberately excluded: control characters other than tab, LF, and a
/// CRLF pair.** The `sqlite3` CLI rewrites those into caret notation
/// (`char(7)` → `^G`, a lone `char(13)` → `^M`) as terminal-safety
/// escaping, while leaving tab, LF, CRLF pairs, and DEL raw. That is a
/// *lossy display* transform belonging to the shell's output layer, not a
/// CSV rule, and whether a file-export tool should reproduce it is an open
/// decision rather than an obvious bug — so this test covers the values
/// where the CSV contract is unambiguous and leaves the escaping question
/// to be settled separately. Tab, LF, CRLF, and DEL *are* covered here,
/// since the oracle passes those through and so must we.
#[test]
fn csv_quote_matches_oracle_on_edge_values() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("csv_quote_matches_oracle_on_edge_values");
        return;
    };

    let dir = scratch_dir("edge-values");
    let db = dir.join("edge.db");

    // Built via the oracle so the fixture is authoritative sqlite3 output,
    // not something this crate wrote and might have written wrongly.
    let seed = "CREATE TABLE t(v TEXT);\n\
        INSERT INTO t VALUES\n\
        ('plain'),\n\
        ('a b'),\n\
        (' leading'),\n\
        ('trailing '),\n\
        ('mid''quote'),\n\
        ('a''b'),\n\
        ('ends'''),\n\
        ('''starts'),\n\
        (''),\n\
        ('has,comma'),\n\
        ('has\"doublequote'),\n\
        ('embedded' || char(13) || char(10) || 'crlf'),\n\
        ('embedded' || char(10) || 'lf'),\n\
        ('tab' || char(9) || 'separated'),\n\
        ('del' || char(127)),\n\
        ('café'),\n\
        ('日本'),\n\
        ('nbsp' || char(160)),\n\
        ('!#$%&()*+-./:;<=>?@[\\]^_`{|}~');\n";
    let status = Command::new(&oracle)
        .arg(&db)
        .arg(seed)
        .status()
        .unwrap_or_else(|e| panic!("seeding {} via oracle: {e}", db.display()));
    assert!(status.success(), "oracle failed to seed {}", db.display());

    let output = run_cli("export", &db);
    assert!(
        output.status.success(),
        "export of the edge-value database failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let actual = std::fs::read_to_string(dir.join("t_edge.csv")).unwrap();
    let expected = oracle_csv_with_header_output(&oracle, &db, "t", &["v".to_string()]);
    assert_eq!(
        actual, expected,
        "CSV edge-value mismatch against the pinned oracle"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Usage errors are exit code 2, distinct from a fatal read error's 1, so
/// a caller can tell "you invoked me wrongly" from "this database is
/// broken".
#[test]
fn usage_errors_exit_two() {
    for args in [vec![], vec!["dump"], vec!["nonsense", "x.db"]] {
        let output = Command::new(CLI)
            .args(&args)
            .output()
            .unwrap_or_else(|e| panic!("running {CLI} {args:?}: {e}"));
        assert_eq!(
            output.status.code(),
            Some(2),
            "expected exit 2 for usage error {args:?}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
