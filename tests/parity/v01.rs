//! V1 parity: "Open any .sqlite file, extract the data." Restates
//! `tests/corpus/dump_oracle_test.rs`'s existing dump/schema comparisons
//! as *mirror* claims under this suite's five-dimension vocabulary —
//! acceptance and output are exercised here; schema and VM-instruction
//! dimensions don't apply to a read-only dump path and are recorded as
//! skipped rather than omitted, so the per-block dimension count in
//! `make assurance`'s Model section stays honest.
//!
//! See issue #72.

use crate::harness::{discover_fixtures, FAMILIES};
use crate::oracle::{oracle_list_output, pinned_oracle, run_oracle, skip_no_oracle};
use sqlite_rs::dump::dump_database;
use sqlite_rs::format::format_list_value;
use sqlite_rs::vfs::UnixVfs;

#[test]
fn acceptance_and_output_match_across_readable_corpus() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("acceptance_and_output_match_across_readable_corpus");
        return;
    };

    let mut checked = 0usize;
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
                continue;
            }

            let result = dump_database(&UnixVfs, &path).unwrap_or_else(|e| {
                panic!("acceptance dimension: dumping {}: {e}", path.display())
            });

            for table in &result.tables {
                if table.columns.is_empty() {
                    continue;
                }
                let ours: Vec<String> = table
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(format_list_value)
                            .collect::<Vec<_>>()
                            .join("|")
                    })
                    .collect();
                let ours = ours.join("\n") + if ours.is_empty() { "" } else { "\n" };
                let theirs = oracle_list_output(&oracle, &path, &table.name, &table.columns);
                assert_eq!(
                    ours,
                    theirs,
                    "output dimension mismatch for {} / {}",
                    path.display(),
                    table.name
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked > 0,
        "expected at least one table to have been compared"
    );
}

#[test]
fn schema_table_names_match() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("schema_table_names_match");
        return;
    };

    for path in discover_fixtures() {
        if path
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            == Some("invalid")
            || path.file_name().and_then(|n| n.to_str()) == Some("hot_journal.db")
        {
            continue;
        }

        let ours = dump_database(&UnixVfs, &path)
            .unwrap_or_else(|e| panic!("schema dimension: dumping {}: {e}", path.display()));
        let mut our_names: Vec<&str> = ours.tables.iter().map(|t| t.name.as_str()).collect();
        our_names.sort_unstable();

        // V1 dumps b-tree tables only (dump_database skips virtual tables —
        // no storage of their own; those are V11's concern), so the oracle
        // side excludes them too via sqlite_schema's `sql` column, which is
        // NULL for a vtab's own entry and non-NULL for its shadow tables —
        // filtering on `using` in the CREATE TABLE text catches the vtab
        // itself specifically.
        let names_sql = "select name from sqlite_schema where type='table' \
            and sql not like 'CREATE VIRTUAL TABLE%' order by name";
        let theirs = run_oracle(&oracle, &path, &["-list"], names_sql);
        let their_names: Vec<&str> = theirs.lines().filter(|l| !l.is_empty()).collect();

        assert_eq!(
            our_names,
            their_names,
            "schema dimension mismatch for {}",
            path.display()
        );
    }
}
