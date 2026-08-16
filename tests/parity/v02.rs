//! V02 value-block parity mirror (issue #72): acceptance and output
//! dimensions for the single-table `SELECT` surface `sqlite-rs query`
//! compiles and executes (V2 phase 3C/#91 + phase 4A/#95). Schema and
//! VM-instruction dimensions don't apply to a query-output comparison
//! and are recorded as skipped, same discipline as `v01.rs`.

use std::path::Path;
use std::process::Command;

use crate::driver::{run_case, ParityCase};
use crate::oracle::{corpus_dir, pinned_oracle, skip_no_oracle};

/// The binary under test, as built by cargo for this test run.
const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn run_query_cli(db: &Path, sql: &str) -> Result<Vec<String>, String> {
    let output = Command::new(CLI)
        .arg("query")
        .arg(db)
        .arg(sql)
        .output()
        .map_err(|e| format!("running {CLI} query: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

const CASES: &[ParityCase] = &[
    ParityCase {
        name: "select_star",
        sql: "SELECT a, b FROM t",
    },
    ParityCase {
        name: "where_gt",
        sql: "SELECT a FROM t WHERE a > 2990",
    },
    ParityCase {
        name: "order_by_limit",
        sql: "SELECT a FROM t ORDER BY a DESC LIMIT 3",
    },
    ParityCase {
        name: "distinct",
        sql: "SELECT DISTINCT b FROM t WHERE a <= 2",
    },
];

#[test]
fn acceptance_and_output_match_for_single_table_select() {
    if pinned_oracle().is_none() {
        skip_no_oracle("acceptance_and_output_match_for_single_table_select");
        return;
    }
    let db = corpus_dir().join("btrees/table_multipage.db");

    let mine: &dyn Fn(&Path, &str) -> Result<Vec<String>, String> = &run_query_cli;
    let mut checked = 0usize;
    for case in CASES {
        let Some(report) = run_case(&db, case, Some(mine)) else {
            continue;
        };
        assert_eq!(
            report.acceptance,
            crate::driver::DimResult::Match,
            "acceptance dimension mismatch for case {:?}",
            case.name
        );
        assert_eq!(
            report.output,
            crate::driver::DimResult::Match,
            "output dimension mismatch for case {:?}",
            case.name
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "expected at least one case to have been compared"
    );
}
