#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! SELECT->bytecode acceptance (spec 009, the codegen convergence
//! ticket #91): the V2 query corpus
//! (`tests/corpus/sql/valid_in_subset/`) compiled and executed against
//! a real `t(a, b, name)` fixture, cross-checked byte-for-byte against
//! the pinned oracle's own row output — reusing
//! `tests/corpus/parser_oracle_test.rs`'s scratch-db-plus-oracle
//! pattern rather than inventing a new harness.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use sqlite_rs::codegen::compile_select;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::record::Value;
use sqlite_rs::schema::TableSchema;
use sqlite_rs::vdbe::execute_with_db;
use sqlite_rs::vfs::{UnixVfs, Vfs, VfsPageSource};

fn pinned_oracle() -> Option<PathBuf> {
    let path = PathBuf::from("sqlite3");
    Command::new(&path).arg("-version").output().ok()?;
    Some(path)
}

fn scratch_fixture() -> (PathBuf, TableSchema) {
    scratch_fixture_labeled("default")
}

fn scratch_fixture_labeled(label: &str) -> (PathBuf, TableSchema) {
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_codegen_select_test_{}_{}.db",
        std::process::id(),
        label
    ));
    let _ = std::fs::remove_file(&path);
    let status = Command::new("sqlite3")
        .arg(&path)
        .arg(
            "CREATE TABLE t(a INTEGER, b INTEGER, name TEXT); \
             INSERT INTO t VALUES (1, 10, 'aa'), (2, 5, 'bb'), (3, 20, 'cc');",
        )
        .status()
        .expect("creating scratch fixture db");
    assert!(status.success());
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 2,
        columns: vec!["a".to_string(), "b".to_string(), "name".to_string()],
        column_types: vec![
            "INTEGER".to_string(),
            "INTEGER".to_string(),
            "TEXT".to_string(),
        ],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: String::new(),
        indexes: vec![],
    };
    (path, schema)
}

fn our_rows(path: &Path, schema: &TableSchema, sql: &str) -> Option<Vec<Vec<Value>>> {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        ParseOutcome::Unsupported { .. } | ParseOutcome::Invalid { .. } => return None,
    };
    let program = compile_select(&select, schema).ok()?;
    let vfs = UnixVfs;
    let file = vfs.open_read(path).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    let source = VfsPageSource::open(&vfs, path, header.page_size).unwrap();
    execute_with_db(&program, Rc::new(source), header).ok()
}

fn oracle_rows(oracle: &Path, db: &Path, sql: &str) -> Vec<Vec<String>> {
    let output = Command::new(oracle)
        .arg("-readonly")
        .arg("-separator")
        .arg("\u{1f}")
        .arg(db)
        .arg(sql)
        .output()
        .expect("invoking sqlite3 oracle");
    // Do not filter empty lines: a single-NULL-column row renders as an
    // empty line from the CLI and is a real row, not a separator
    // artifact — `str::lines` already excludes any trailing newline's
    // phantom empty entry.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.split('\u{1f}').map(str::to_string).collect())
        .collect()
}

fn value_to_oracle_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => {
            if r.fract() == 0.0 {
                format!("{r:.1}")
            } else {
                r.to_string()
            }
        }
        Value::Text(s) => s.clone(),
        Value::Blob(_) => "<blob>".to_string(),
    }
}

/// Compiles and executes every statement in `tests/corpus/sql/valid_in_subset/`
/// against the `t(a, b, name)` fixture, comparing our output row-for-row
/// against the pinned oracle wherever both our parser and codegen
/// accept the statement — skipping (not failing) statements our V2
/// slice doesn't compile (out-of-scope constructs are tracked as
/// documented gaps in `src/codegen/expr.rs`'s doc comments, not silent
/// failures here).
#[test]
fn v2_corpus_compiles_and_matches_oracle_row_for_row() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!(
            "skipping v2_corpus_compiles_and_matches_oracle_row_for_row: no sqlite3 oracle on PATH"
        );
        return;
    };
    let (path, schema) = scratch_fixture();
    let sql_dir = Path::new("tests/corpus/sql/valid_in_subset");
    let mut files: Vec<PathBuf> = std::fs::read_dir(sql_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("sql"))
        .collect();
    files.sort();
    assert!(!files.is_empty());

    let mut compiled = 0usize;
    let mut matched = 0usize;
    let mut mismatches = Vec::new();
    for file in files {
        let content = std::fs::read_to_string(&file).unwrap();
        for stmt in content.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if KNOWN_GAPS.iter().any(|g| stmt.contains(g)) {
                continue;
            }
            let Some(our) = our_rows(&path, &schema, stmt) else {
                continue;
            };
            compiled += 1;
            let oracle_out = oracle_rows(&oracle, &path, stmt);
            let our_text: Vec<Vec<String>> = our
                .iter()
                .map(|row| row.iter().map(value_to_oracle_text).collect())
                .collect();
            if our_text != oracle_out {
                mismatches.push(format!("{stmt:?}: ours={our_text:?} oracle={oracle_out:?}"));
                continue;
            }
            matched += 1;
        }
    }
    assert!(
        compiled >= 10,
        "expected a meaningful slice of the V2 corpus to compile through codegen, only {compiled} did"
    );
    assert!(
        mismatches.is_empty(),
        "{} unexpected mismatch(es) (not in KNOWN_GAPS):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    assert_eq!(compiled, matched);
}

/// Corpus statements this ticket's codegen is known not to reproduce
/// oracle-exactly yet. Empty as of #142: bitwise/concat opcodes landed
/// in #139, and CAST/REAL-literal fidelity (the last two live entries,
/// `CAST(name AS REAL)` and `a = 1.;`) landed in #142.
const KNOWN_GAPS: &[&str] = &[];

/// Regression fixture for #141: two computed result columns, each of
/// which allocates temporaries before its own destination register,
/// used to collide against `compile_row_values`'s contiguous-register
/// assumption and be rejected outright as unsupported.
#[test]
fn two_computed_result_columns_do_not_collide() {
    let (path, schema) = scratch_fixture_labeled("copy_arith");
    let rows = our_rows(&path, &schema, "SELECT a + 1, a - 1 FROM t")
        .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(2), Value::Integer(0)],
            vec![Value::Integer(3), Value::Integer(1)],
            vec![Value::Integer(4), Value::Integer(2)],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn coalesce_and_ifnull_result_columns_do_not_collide() {
    let (path, schema) = scratch_fixture_labeled("copy_func");
    let rows = our_rows(
        &path,
        &schema,
        "SELECT coalesce(a, -1), ifnull(name, 'z') FROM t",
    )
    .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(1), Value::Text("aa".to_string())],
            vec![Value::Integer(2), Value::Text("bb".to_string())],
            vec![Value::Integer(3), Value::Text("cc".to_string())],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

/// Regression fixture for #140: `ORDER BY ... NULLS FIRST/LAST` was
/// parsed and stored (`ast::OrderingTerm::nulls_last`) but never read by
/// `resolve_order_by`, so the explicit modifier was silently ignored.
fn nulls_fixture(label: &str) -> (PathBuf, TableSchema) {
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_codegen_select_nulls_test_{}_{}.db",
        std::process::id(),
        label
    ));
    let _ = std::fs::remove_file(&path);
    let status = Command::new("sqlite3")
        .arg(&path)
        .arg(
            "CREATE TABLE t(i INTEGER); \
             INSERT INTO t VALUES (5), (NULL), (-7), (0), (5);",
        )
        .status()
        .expect("creating nulls fixture db");
    assert!(status.success());
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 2,
        columns: vec!["i".to_string()],
        column_types: vec!["INTEGER".to_string()],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: String::new(),
        indexes: vec![],
    };
    (path, schema)
}

#[test]
fn order_by_asc_nulls_last_matches_oracle() {
    let (path, schema) = nulls_fixture("asc_nulls_last");
    let rows = our_rows(&path, &schema, "SELECT i FROM t ORDER BY i ASC NULLS LAST;")
        .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(-7)],
            vec![Value::Integer(0)],
            vec![Value::Integer(5)],
            vec![Value::Integer(5)],
            vec![Value::Null],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn order_by_desc_nulls_first_matches_oracle() {
    let (path, schema) = nulls_fixture("desc_nulls_first");
    let rows = our_rows(
        &path,
        &schema,
        "SELECT i FROM t ORDER BY i DESC NULLS FIRST;",
    )
    .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Null],
            vec![Value::Integer(5)],
            vec![Value::Integer(5)],
            vec![Value::Integer(0)],
            vec![Value::Integer(-7)],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn order_by_asc_default_places_nulls_first() {
    let (path, schema) = nulls_fixture("asc_default");
    let rows = our_rows(&path, &schema, "SELECT i FROM t ORDER BY i ASC;")
        .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Null],
            vec![Value::Integer(-7)],
            vec![Value::Integer(0)],
            vec![Value::Integer(5)],
            vec![Value::Integer(5)],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn order_by_desc_default_places_nulls_last() {
    let (path, schema) = nulls_fixture("desc_default");
    let rows = our_rows(&path, &schema, "SELECT i FROM t ORDER BY i DESC;")
        .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(5)],
            vec![Value::Integer(5)],
            vec![Value::Integer(0)],
            vec![Value::Integer(-7)],
            vec![Value::Null],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

/// #144: `ORDER BY <ordinal>` resolves 1-based against the result-column
/// list, same as `sqlite3`.
#[test]
fn order_by_ordinal_resolves_result_column() {
    let (path, schema) = scratch_fixture_labeled("ordinal");
    let rows = our_rows(&path, &schema, "SELECT a, b FROM t ORDER BY 2 DESC;")
        .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(3), Value::Integer(20)],
            vec![Value::Integer(1), Value::Integer(10)],
            vec![Value::Integer(2), Value::Integer(5)],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

/// #144: an out-of-range ordinal is rejected rather than silently
/// wrapping or panicking.
#[test]
fn order_by_ordinal_out_of_range_is_rejected() {
    let (path, schema) = scratch_fixture_labeled("ordinal_oor");
    assert!(our_rows(&path, &schema, "SELECT a, b FROM t ORDER BY 3;").is_none());
    let _ = std::fs::remove_file(&path);
}

/// #144: `ORDER BY <alias>` resolves against the result-column alias
/// before falling back to a table column of the same name.
#[test]
fn order_by_alias_resolves_result_column() {
    let (path, schema) = scratch_fixture_labeled("alias");
    let rows = our_rows(&path, &schema, "SELECT a, b AS x FROM t ORDER BY x DESC;")
        .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(3), Value::Integer(20)],
            vec![Value::Integer(1), Value::Integer(10)],
            vec![Value::Integer(2), Value::Integer(5)],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

/// #144: an unknown alias/column name in ORDER BY is still rejected.
#[test]
fn order_by_unknown_name_is_rejected() {
    let (path, schema) = scratch_fixture_labeled("unknown_name");
    assert!(our_rows(&path, &schema, "SELECT a FROM t ORDER BY nope;").is_none());
    let _ = std::fs::remove_file(&path);
}

/// #144: `ORDER BY ... COLLATE NOCASE` is honoured rather than always
/// comparing under BINARY.
#[test]
fn order_by_collate_nocase_is_case_insensitive() {
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_codegen_select_collate_test_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let status = Command::new("sqlite3")
        .arg(&path)
        .arg(
            "CREATE TABLE t(name TEXT); \
             INSERT INTO t VALUES ('bb'), ('AA'), ('cc');",
        )
        .status()
        .expect("creating collate fixture db");
    assert!(status.success());
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 2,
        columns: vec!["name".to_string()],
        column_types: vec!["TEXT".to_string()],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: String::new(),
        indexes: vec![],
    };
    let rows = our_rows(
        &path,
        &schema,
        "SELECT name FROM t ORDER BY name COLLATE NOCASE;",
    )
    .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Text("AA".to_string())],
            vec![Value::Text("bb".to_string())],
            vec![Value::Text("cc".to_string())],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

/// #155: `ORDER BY <unary expr>` sorts by the computed value, not the
/// raw column — descending by `a` here since `-a` is ascending exactly
/// when `a` is descending.
#[test]
fn order_by_unary_expression_matches_oracle() {
    let (path, schema) = scratch_fixture_labeled("unary_expr");
    let rows = our_rows(&path, &schema, "SELECT a FROM t ORDER BY -a;")
        .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(3)],
            vec![Value::Integer(2)],
            vec![Value::Integer(1)],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

/// #155: `ORDER BY <binary expr>` over a column.
#[test]
fn order_by_binary_expression_matches_oracle() {
    let (path, schema) = scratch_fixture_labeled("binary_expr");
    let rows = our_rows(&path, &schema, "SELECT a, b FROM t ORDER BY b - a;")
        .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(2), Value::Integer(5)],
            vec![Value::Integer(1), Value::Integer(10)],
            vec![Value::Integer(3), Value::Integer(20)],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

/// #155: `ORDER BY <scalar function call>` over a column.
#[test]
fn order_by_function_call_matches_oracle() {
    let (path, schema) = scratch_fixture_labeled("function_expr");
    let rows = our_rows(
        &path,
        &schema,
        "SELECT name FROM t ORDER BY lower(name) DESC;",
    )
    .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Text("cc".to_string())],
            vec![Value::Text("bb".to_string())],
            vec![Value::Text("aa".to_string())],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

/// #155: an alias whose own result expression is computed (not a bare
/// column) is still usable as an ORDER BY target, resolving to that
/// underlying expression rather than being refused. A single computed
/// result column is used (rather than mixing it with a bare column) to
/// stay clear of `compile_row_values`'s separate, pre-existing
/// contiguous-registers limitation on mixed column/expression
/// projections.
#[test]
fn order_by_alias_to_computed_expression_matches_oracle() {
    let (path, schema) = scratch_fixture_labeled("alias_computed");
    let rows = our_rows(&path, &schema, "SELECT -a AS neg FROM t ORDER BY neg;")
        .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(-3)],
            vec![Value::Integer(-2)],
            vec![Value::Integer(-1)],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

/// #155: a computed ORDER BY expression composes with LIMIT/OFFSET and
/// a second, plain-column sort key.
#[test]
fn order_by_expression_with_limit_offset_and_second_key() {
    let (path, schema) = scratch_fixture_labeled("expr_limit_offset");
    let rows = our_rows(
        &path,
        &schema,
        "SELECT a, b FROM t ORDER BY -a, b LIMIT 2 OFFSET 1;",
    )
    .expect("query should compile and execute");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(2), Value::Integer(5)],
            vec![Value::Integer(1), Value::Integer(10)],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

/// #137: `WHERE rowid = <int literal>` compiles to `SeekRowid` — no
/// `Rewind`/`Next` full-table scan — and still answers the same row
/// the ordinary scan would.
#[test]
fn rowid_equality_seeks_and_matches_oracle() {
    let (path, schema) = scratch_fixture_labeled("seek_rowid");
    let select = match parse_select("SELECT a, b, name FROM t WHERE rowid = 2;") {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected the parser to accept this query, got {other:?}"),
    };
    let program = compile_select(&select, &schema).expect("compiles");
    let rows = sqlite_rs::vdbe::explain(&program);
    assert!(
        rows.iter().any(|r| r.opcode == "SeekRowid"),
        "expected SeekRowid in the compiled program: {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.opcode == "Rewind"),
        "SeekRowid fast path must not also emit a Rewind/Next scan: {rows:?}"
    );

    let our = our_rows(&path, &schema, "SELECT a, b, name FROM t WHERE rowid = 2;")
        .expect("query should compile and execute");
    if let Some(oracle) = pinned_oracle() {
        let oracle_out = oracle_rows(&oracle, &path, "SELECT a, b, name FROM t WHERE rowid = 2;");
        let ours_as_text: Vec<Vec<String>> = our
            .iter()
            .map(|row| row.iter().map(value_to_oracle_text).collect())
            .collect();
        assert_eq!(ours_as_text, oracle_out);
    } else {
        assert_eq!(
            our,
            vec![vec![
                Value::Integer(2),
                Value::Integer(5),
                Value::Text("bb".to_string())
            ]]
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// #137: a rowid seek by a missing rowid returns no rows (the
/// `SeekRowid` not-found jump lands cleanly on `Halt`, not a panic or a
/// phantom row).
#[test]
fn rowid_equality_seek_missing_row_returns_empty() {
    let (path, schema) = scratch_fixture_labeled("seek_rowid_missing");
    let rows = our_rows(&path, &schema, "SELECT a FROM t WHERE rowid = 999;")
        .expect("query should compile and execute");
    assert!(rows.is_empty());
    let _ = std::fs::remove_file(&path);
}

/// #137: `WHERE rowid = ?` compiles to `Variable` + `SeekRowid`, and
/// the bound parameter value actually drives the seek (exercising the
/// bind-parameter plumbing added alongside the fast path, not just the
/// opcode shape).
#[test]
fn rowid_equality_against_bound_parameter_seeks() {
    let select = match parse_select("SELECT a, name FROM t WHERE rowid = ?;") {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected the parser to accept this query, got {other:?}"),
    };
    let (path, schema) = scratch_fixture_labeled("seek_rowid_param");
    let program = compile_select(&select, &schema).expect("compiles");
    let rows = sqlite_rs::vdbe::explain(&program);
    assert!(rows.iter().any(|r| r.opcode == "Variable"));
    assert!(rows.iter().any(|r| r.opcode == "SeekRowid"));

    let vfs = UnixVfs;
    let file = vfs.open_read(&path).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    let source = VfsPageSource::open(&vfs, &path, header.page_size).unwrap();
    let result = sqlite_rs::vdbe::execute_with_db_and_params(
        &program,
        Rc::new(source),
        header,
        vec![Value::Integer(3)],
    )
    .expect("executes");
    assert_eq!(
        result,
        vec![vec![Value::Integer(3), Value::Text("cc".to_string())]]
    );
    let _ = std::fs::remove_file(&path);
}

/// #239: `GROUP BY` / `HAVING` fixture — `cat` groups rows unevenly (2
/// `"x"`, 1 `"y"`, 3 `"z"`) with an interspersed `NULL` `val`, exercising
/// `count`/`sum`/`avg`/`min`/`max`, multi-column grouping, `HAVING`, and
/// `GROUP BY` over a computed expression.
fn group_by_fixture(label: &str) -> (PathBuf, TableSchema) {
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_codegen_select_group_by_test_{}_{}.db",
        std::process::id(),
        label
    ));
    let _ = std::fs::remove_file(&path);
    let status = Command::new("sqlite3")
        .arg(&path)
        .arg(
            "CREATE TABLE t(cat TEXT, sub TEXT, val INTEGER); \
             INSERT INTO t VALUES \
             ('x', 'p', 1), ('x', 'p', 2), \
             ('y', 'p', 10), \
             ('z', 'q', 100), ('z', 'q', NULL), ('z', 'r', 5);",
        )
        .status()
        .expect("creating GROUP BY fixture db");
    assert!(status.success());
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 2,
        columns: vec!["cat".to_string(), "sub".to_string(), "val".to_string()],
        column_types: vec![
            "TEXT".to_string(),
            "TEXT".to_string(),
            "INTEGER".to_string(),
        ],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: String::new(),
        indexes: vec![],
    };
    (path, schema)
}

#[test]
fn group_by_single_column_count_matches_oracle() {
    let (path, schema) = group_by_fixture("single_count");
    // #239 doesn't compile GROUP BY combined with ORDER BY in one
    // `SELECT`; sort the expected rows in Rust instead of pushing that
    // combination through codegen.
    let mut rows = our_rows(&path, &schema, "SELECT cat, count(*) FROM t GROUP BY cat;")
        .expect("query should compile and execute");
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(
        rows,
        vec![
            vec![Value::Text("x".to_string()), Value::Integer(2)],
            vec![Value::Text("y".to_string()), Value::Integer(1)],
            vec![Value::Text("z".to_string()), Value::Integer(3)],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn group_by_aggregates_sum_avg_min_max() {
    let (path, schema) = group_by_fixture("aggregates");
    let mut rows = our_rows(
        &path,
        &schema,
        "SELECT cat, sum(val), avg(val), min(val), max(val) FROM t GROUP BY cat;",
    )
    .expect("query should compile and execute");
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(
        rows,
        vec![
            vec![
                Value::Text("x".to_string()),
                Value::Integer(3),
                Value::Real(1.5),
                Value::Integer(1),
                Value::Integer(2),
            ],
            vec![
                Value::Text("y".to_string()),
                Value::Integer(10),
                Value::Real(10.0),
                Value::Integer(10),
                Value::Integer(10),
            ],
            // `z` has a NULL `val` row: sum/avg/min/max all ignore it,
            // matching SQL's null-skipping aggregate semantics.
            vec![
                Value::Text("z".to_string()),
                Value::Integer(105),
                Value::Real(52.5),
                Value::Integer(5),
                Value::Integer(100),
            ],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn group_by_multiple_columns() {
    let (path, schema) = group_by_fixture("multi_column");
    let mut rows = our_rows(
        &path,
        &schema,
        "SELECT cat, sub, count(*) FROM t GROUP BY cat, sub;",
    )
    .expect("query should compile and execute");
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(
        rows,
        vec![
            vec![
                Value::Text("x".to_string()),
                Value::Text("p".to_string()),
                Value::Integer(2)
            ],
            vec![
                Value::Text("y".to_string()),
                Value::Text("p".to_string()),
                Value::Integer(1)
            ],
            vec![
                Value::Text("z".to_string()),
                Value::Text("q".to_string()),
                Value::Integer(2)
            ],
            vec![
                Value::Text("z".to_string()),
                Value::Text("r".to_string()),
                Value::Integer(1)
            ],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn group_by_having_filters_groups() {
    let (path, schema) = group_by_fixture("having");
    let mut rows = our_rows(
        &path,
        &schema,
        "SELECT cat, count(*) FROM t GROUP BY cat HAVING count(*) > 1;",
    )
    .expect("query should compile and execute");
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(
        rows,
        vec![
            vec![Value::Text("x".to_string()), Value::Integer(2)],
            vec![Value::Text("z".to_string()), Value::Integer(3)],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn group_by_expression() {
    let (path, schema) = group_by_fixture("expression");
    let mut rows = our_rows(
        &path,
        &schema,
        "SELECT length(cat), count(*) FROM t GROUP BY length(cat);",
    )
    .expect("query should compile and execute");
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    // Every `cat` value in the fixture is a single character, so
    // `length(cat)` groups all six rows into one bucket.
    assert_eq!(rows, vec![vec![Value::Integer(1), Value::Integer(6)]]);
    let _ = std::fs::remove_file(&path);
}
