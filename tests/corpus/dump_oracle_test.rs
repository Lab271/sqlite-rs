//! Byte-for-byte diff of `dump_database`'s rendering against a real,
//! read-only `sqlite3` process — issue #37's "match `sqlite3`
//! `-csv`/`-list` modes exactly" acceptance bar, exercised across every
//! table of every non-`invalid`, non-hot-journal corpus fixture.
//!
//! Deliberately NOT in `harness.rs`/`oracle.rs`: those modules'
//! documented design is "never shell out to `sqlite3`, read only
//! committed fixtures." This file is a scoped, explicit exception —
//! read-only (`-readonly`, never opens for write, so it cannot mutate a
//! committed fixture the way an accidental read-write open could) and
//! skipped entirely (not failed) if no `sqlite3` binary is on `PATH`, so
//! a machine without one still passes `make test-corpus`.

use crate::harness::{discover_fixtures, FAMILIES};
use sqlite_rs::dump::dump_database;
use sqlite_rs::format::{format_csv_value, format_list_value};
use sqlite_rs::vfs::UnixVfs;
use std::path::Path;
use std::process::Command;

fn sqlite3_available() -> bool {
    Command::new("sqlite3")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The oracle's own value for one column: `quote(col)` for a BLOB-typed
/// column (list/csv mode can't print raw bytes at all), the column
/// itself otherwise. Determined per-row via `typeof()`, since a column's
/// *declared* type doesn't guarantee every row's dynamic storage class
/// (e.g. the serial-type-8/9 REAL-as-integer-constant case this crate's
/// own `dump` module already accounts for).
fn oracle_list_output(db: &Path, table: &str, columns: &[String]) -> String {
    let select_list = columns
        .iter()
        .map(|c| format!("(case when typeof(\"{c}\")='blob' then quote(\"{c}\") else \"{c}\" end)"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("select {select_list} from \"{table}\"");
    let output = Command::new("sqlite3")
        .arg("-readonly")
        .arg("-list")
        .arg("-separator")
        .arg("|")
        .arg("-nullvalue")
        .arg("NULL")
        .arg(db)
        .arg(&sql)
        .output()
        .unwrap_or_else(|e| panic!("running sqlite3 oracle on {}: {e}", db.display()));
    assert!(
        output.status.success(),
        "sqlite3 oracle failed on {} table {table}: {}",
        db.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn oracle_csv_output(db: &Path, table: &str, columns: &[String]) -> String {
    let select_list = columns
        .iter()
        .map(|c| format!("(case when typeof(\"{c}\")='blob' then quote(\"{c}\") else \"{c}\" end)"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("select {select_list} from \"{table}\"");
    let output = Command::new("sqlite3")
        .arg("-readonly")
        .arg("-csv")
        .arg(db)
        .arg(&sql)
        .output()
        .unwrap_or_else(|e| panic!("running sqlite3 oracle on {}: {e}", db.display()));
    assert!(
        output.status.success(),
        "sqlite3 oracle failed on {} table {table}: {}",
        db.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn dump_matches_sqlite3_list_mode_across_corpus() {
    if !sqlite3_available() {
        eprintln!("skipping: no sqlite3 on PATH");
        return;
    }

    let invalid_or_hot_journal = |p: &std::path::Path| {
        let in_invalid = p
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            == Some("invalid");
        in_invalid || p.file_name().and_then(|n| n.to_str()) == Some("hot_journal.db")
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
                == Some(family)
        }) {
            if invalid_or_hot_journal(&path) {
                continue;
            }
            let result = dump_database(&UnixVfs, &path)
                .unwrap_or_else(|e| panic!("dumping {}: {e}", path.display()));

            for table in &result.tables {
                if table.columns.is_empty() {
                    eprintln!(
                        "skipping oracle diff for {} / {} — no parsed column names",
                        path.display(),
                        table.name
                    );
                    continue;
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
                let oracle_list = oracle_list_output(&path, &table.name, &table.columns);
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
                let mine_csv = mine_csv.join("\n") + if mine_csv.is_empty() { "" } else { "\n" };
                let oracle_csv = oracle_csv_output(&path, &table.name, &table.columns);
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
    eprintln!("dump_matches_sqlite3_list_mode_across_corpus: {checked_tables} tables checked");
}
