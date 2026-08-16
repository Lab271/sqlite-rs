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
    // #144: ORDER BY ordinals and result-column aliases, not just bare
    // table-column references.
    ParityCase {
        name: "order_by_ordinal",
        sql: "SELECT a, b FROM t ORDER BY 2 DESC LIMIT 3",
    },
    ParityCase {
        name: "order_by_alias",
        sql: "SELECT a, b AS x FROM t ORDER BY x DESC LIMIT 3",
    },
];

/// Three-valued logic over a fixture that actually contains NULLs
/// (#134). Every case is a shape whose answer changes depending on
/// whether SQL's *unknown* is resolved honestly or folded into true.
///
/// The value-mode cases select the boolean/NULL expression directly —
/// previously they were wrapped in `IS NULL` to dodge a renderer bug
/// where the CLI printed a `NULL` literal instead of the shell's default
/// empty string (#143); now that `query`'s `-list` output matches the
/// shell's NULL spelling, the raw value can be compared.
const NULL_CASES: &[ParityCase] = &[
    ParityCase {
        name: "not_over_eq_excludes_null",
        sql: "SELECT i FROM t WHERE NOT (i = 0)",
    },
    ParityCase {
        name: "ne_excludes_null",
        sql: "SELECT i FROM t WHERE i <> 0",
    },
    ParityCase {
        name: "not_over_in",
        sql: "SELECT i FROM t WHERE NOT (i IN (0, 127))",
    },
    ParityCase {
        name: "not_in_spelling",
        sql: "SELECT i FROM t WHERE i NOT IN (0, 127)",
    },
    ParityCase {
        name: "not_over_between",
        sql: "SELECT i FROM t WHERE NOT (i BETWEEN -1 AND 1)",
    },
    ParityCase {
        name: "not_over_and",
        sql: "SELECT i FROM t WHERE NOT (i = 0 AND txt = 'hello')",
    },
    ParityCase {
        name: "not_over_or",
        sql: "SELECT i FROM t WHERE NOT (i = 0 OR txt = 'hello')",
    },
    ParityCase {
        name: "not_over_is_null",
        sql: "SELECT i FROM t WHERE NOT (txt IS NULL)",
    },
    ParityCase {
        name: "double_negation",
        sql: "SELECT i FROM t WHERE NOT NOT (i = 1)",
    },
    ParityCase {
        name: "value_eq",
        sql: "SELECT i = 0 FROM t",
    },
    ParityCase {
        name: "value_not",
        sql: "SELECT NOT i FROM t",
    },
    ParityCase {
        name: "value_in",
        sql: "SELECT i IN (0, 127) FROM t",
    },
    ParityCase {
        name: "value_not_in",
        sql: "SELECT i NOT IN (0, 127) FROM t",
    },
    ParityCase {
        name: "value_between",
        sql: "SELECT i BETWEEN 0 AND 1 FROM t",
    },
];

/// `SELECT *` over `serialtypes/values.db`'s `t(i, r, txt, blb)`: a REAL
/// column, NULLs, and a BLOB with non-UTF-8 bytes all in one row set —
/// the exact shape #143's renderer bug (`NULL` literal, `X'HEX'` blobs,
/// bare-integer REALs) would have broken byte-identical output for.
const REAL_STAR_CASES: &[ParityCase] = &[ParityCase {
    name: "star_expansion_with_real_and_blob_columns",
    sql: "SELECT * FROM t",
}];

#[test]
fn star_expansion_acceptance_and_output_match_for_a_real_column_table() {
    if pinned_oracle().is_none() {
        skip_no_oracle("star_expansion_acceptance_and_output_match_for_a_real_column_table");
        return;
    }
    assert_cases_match(&corpus_dir().join("serialtypes/values.db"), REAL_STAR_CASES);
}

/// `SELECT *` over a table with an `INTEGER PRIMARY KEY`. That column
/// is a NULL placeholder in every record and has to be read with
/// `Rowid`, not `Column` — the star-expansion path did the latter, so
/// `SELECT * FROM t` answered NULL for it while `SELECT id FROM t`
/// answered correctly.
///
/// The fixture is an FTS5 shadow table because no corpus fixture is a
/// plain table with an `INTEGER PRIMARY KEY` — which is exactly why the
/// oracle suites never caught this. `t_content` is an ordinary rowid
/// table despite its provenance, and it is the only one available;
/// giving the corpus a first-class fixture for this shape is tracked
/// separately.
const ROWID_ALIAS_CASES: &[ParityCase] = &[
    ParityCase {
        name: "star_expands_rowid_alias",
        sql: "SELECT * FROM t_content",
    },
    ParityCase {
        name: "qualified_star_expands_rowid_alias",
        sql: "SELECT t_content.* FROM t_content",
    },
    ParityCase {
        name: "named_rowid_alias_agrees_with_star",
        sql: "SELECT id, c0 FROM t_content",
    },
];

#[test]
fn star_expansion_acceptance_and_output_match_for_a_rowid_alias_table() {
    if pinned_oracle().is_none() {
        skip_no_oracle("star_expansion_acceptance_and_output_match_for_a_rowid_alias_table");
        return;
    }
    assert_cases_match(&corpus_dir().join("features/fts5.db"), ROWID_ALIAS_CASES);
}

fn assert_cases_match(db: &Path, cases: &[ParityCase]) {
    let mine: &dyn Fn(&Path, &str) -> Result<Vec<String>, String> = &run_query_cli;
    let mut checked = 0usize;
    for case in cases {
        let Some(report) = run_case(db, case, Some(mine)) else {
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

#[test]
fn three_valued_logic_acceptance_and_output_match_over_null_rows() {
    if pinned_oracle().is_none() {
        skip_no_oracle("three_valued_logic_acceptance_and_output_match_over_null_rows");
        return;
    }
    // `btrees/table_multipage.db` has no NULL in any column, so every
    // case above would be vacuous there; `serialtypes/values.db` is the
    // fixture with NULL rows in both an INTEGER and a TEXT column.
    assert_cases_match(&corpus_dir().join("serialtypes/values.db"), NULL_CASES);
}

#[test]
fn acceptance_and_output_match_for_single_table_select() {
    if pinned_oracle().is_none() {
        skip_no_oracle("acceptance_and_output_match_for_single_table_select");
        return;
    }
    assert_cases_match(&corpus_dir().join("btrees/table_multipage.db"), CASES);
}
