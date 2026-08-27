// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Byte-for-byte diff of `dump_database`'s rendering against a real,
//! read-only `sqlite3` process — issue #37's "match `sqlite3`
//! `-csv`/`-list` modes exactly" acceptance bar, exercised across every
//! table of every non-`invalid`, non-hot-journal corpus fixture.
//!
//! Deliberately NOT in `harness.rs`: that module's documented design is
//! "never shell out to `sqlite3`, read only committed fixtures." Live
//! oracle invocation is a scoped, explicit exception, and the helpers for
//! it live in `oracle.rs` (shared with `cli_e2e_test.rs`) — always
//! `-readonly`, so the oracle cannot mutate a committed fixture the way
//! an accidental read-write open could, and skipped entirely (not failed)
//! when no `sqlite3` binary is on `PATH`, so a machine without one still
//! passes `make test-corpus`.
//!
//! This file compares at the *library* level (`dump_database` plus the
//! `format_*` functions). `cli_e2e_test.rs` covers the same ground
//! through the actual CLI binary, where the header row, file naming, and
//! exit codes also come into play (#55).

use crate::harness::{discover_fixtures, FAMILIES};
use crate::oracle::{oracle_csv_output, oracle_list_output, pinned_oracle, skip_no_oracle};
use sqlite_rs::dump::dump_database;
use sqlite_rs::format::{format_csv_value, format_list_value};
use sqlite_rs::vfs::UnixVfs;

#[test]
fn dump_matches_sqlite3_list_mode_across_corpus() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("dump_matches_sqlite3_list_mode_across_corpus");
        return;
    };

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
                let oracle_list = oracle_list_output(&oracle, &path, &table.name, &table.columns);
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
                // `-csv` mode terminates rows with CRLF (unlike `-list`'s bare LF)
                // — see `CSV_ROW_TERMINATOR` in `src/bin/sqlite-rs.rs`.
                let mine_csv = mine_csv
                    .iter()
                    .map(|line| format!("{line}\r\n"))
                    .collect::<String>();
                let oracle_csv = oracle_csv_output(&oracle, &path, &table.name, &table.columns);
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
