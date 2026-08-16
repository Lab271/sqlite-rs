//! Executes one vendored `.test` file's [`Record`]s: `statement ok`
//! blocks are replayed through the pinned oracle (read-write) to build
//! real on-disk fixture state — this engine has no write path yet, see
//! `.openspec/plan.md` V2 — and `query` blocks run through the same
//! read pipeline `sqlite-rs query` uses (`parse_select` ->
//! `read_schema` -> `compile_select` -> `execute_with_db`), scored
//! against the file's own expected block. `statement error` blocks are
//! neither replayed nor scored: they probe the oracle's own rejection
//! behavior, which this runner isn't validating.
//!
//! Skip-not-fail (spec 004 Requirement 4): grammar/feature gaps our V2
//! slice doesn't cover yet (unparsed syntax, multi-table/view `FROM`,
//! unimplemented opcodes) are skipped, not failed. A `query` that our
//! pipeline accepts, compiles, and executes but whose *result* diverges
//! from the file's expected block is a real bug — that counts as fail.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use md5::{Digest, Md5};

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{compile_select, CodegenError};
use sqlite_rs::dump;
use sqlite_rs::format::{format_blob, format_real};
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::record::Value;
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::{execute_with_db, ExecError};
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::format::{Expected, QueryRecord, Record, SortMode};

pub struct FileTally {
    pub file: String,
    pub pass: usize,
    pub skip: usize,
    pub fail: usize,
    /// One line per fail, `<line>: <reason>` — kept short, printed by
    /// the caller for triage.
    pub failures: Vec<String>,
}

/// Runs `sqlite3 <db_path> <sql>` read-write, to build fixture state a
/// `statement ok` block describes. Not [`crate::oracle::run_oracle`]:
/// that helper is always `-readonly` by design (it guards *committed*
/// fixtures from mutation), whereas this runner's db is a disposable
/// scratch file it owns end to end.
fn oracle_exec_write(oracle: &Path, db_path: &Path, sql: &str) {
    let output = Command::new(oracle)
        .arg(db_path)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running sqlite3 oracle on {}: {e}", db_path.display()));
    assert!(
        output.status.success(),
        "oracle setup failed on {}: {}\nsql:\n{sql}",
        db_path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Renders one column value the way the sqllogictest reference tool
/// does: `NULL` for null, `(empty)` for an empty string, and — only for
/// columns declared `R` — a fixed 3-decimal form regardless of the
/// value's actual precision. Verified against the pinned oracle via
/// `tests/corpus/sql/vendor/sqllogictest/test/evidence/slt_lang_aggfunc.test`,
/// whose `avg(x)` result (exactly representable as `1.25`) is committed
/// as the literal `1.250`, not `1.25` — ruling out this crate's own
/// `%.15g`-style [`format_real`] for `R` columns specifically.
fn sqllogictest_value(value: &Value, type_letter: char) -> String {
    if type_letter == 'R' {
        let as_f64 = match value {
            Value::Null => return "NULL".to_string(),
            Value::Integer(i) => *i as f64,
            Value::Real(r) => *r,
            Value::Text(s) => s.parse().unwrap_or(0.0),
            Value::Blob(_) => 0.0,
        };
        return format!("{as_f64:.3}");
    }
    match value {
        Value::Null => "NULL".to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format_real(*r),
        Value::Text(s) if s.is_empty() => "(empty)".to_string(),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => format_blob(b),
    }
}

/// Flattens `rows` (each already rendered to its per-column text form)
/// row-major, applying the query's sort mode: `rowsort` reorders whole
/// rows (lexicographic over each row's rendered columns) before
/// flattening, `valuesort` flattens first and then sorts every
/// individual value, `nosort` keeps the engine's own row order.
fn flatten_sorted(mut rows: Vec<Vec<String>>, mode: SortMode) -> Vec<String> {
    match mode {
        SortMode::NoSort => rows.into_iter().flatten().collect(),
        SortMode::RowSort => {
            rows.sort();
            rows.into_iter().flatten().collect()
        }
        SortMode::ValueSort => {
            let mut flat: Vec<String> = rows.into_iter().flatten().collect();
            flat.sort();
            flat
        }
    }
}

enum Outcome {
    Pass,
    Skip,
    Fail(String),
}

fn run_query(db_path: &Path, record: &QueryRecord) -> Outcome {
    let select = match parse_select(&record.sql) {
        ParseOutcome::Accepted(select) => *select,
        ParseOutcome::Unsupported { .. } | ParseOutcome::Invalid { .. } => return Outcome::Skip,
    };
    let Some(from) = &select.from else {
        return Outcome::Skip;
    };

    let (header, pager) = match dump::open(&UnixVfs, db_path) {
        Ok(v) => v,
        Err(e) => return Outcome::Fail(format!("opening fixture db: {e}")),
    };
    let source: Rc<dyn PageSource> = Rc::new(pager);

    let mut schema_cursor = TableCursor::new(Rc::clone(&source), &header, 1);
    let schemas = match read_schema(&mut schema_cursor, header.text_encoding) {
        Ok(s) => s,
        Err(_) => return Outcome::Skip,
    };
    let Some(schema) = schemas
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(&from.name))
    else {
        return Outcome::Skip;
    };

    let program = match compile_select(&select, schema) {
        Ok(p) => p,
        Err(
            CodegenError::NoFromClause
            | CodegenError::UnknownColumn { .. }
            | CodegenError::Unsupported { .. },
        ) => return Outcome::Skip,
    };

    let rows = match execute_with_db(&program, source, header) {
        Ok(r) => r,
        Err(ExecError::Unimplemented { .. }) => return Outcome::Skip,
        // An unknown scalar/aggregate function is a feature gap (V2
        // ships ~20 scalar functions and no aggregates at all), not a
        // divergence — the `Function` opcode reports it as a malformed
        // instruction rather than as its own error variant.
        Err(ExecError::MalformedInstruction {
            opcode: "Function",
            reason,
        }) if reason.starts_with("unknown function") => return Outcome::Skip,
        Err(e) => return Outcome::Fail(format!("executing: {e}")),
    };

    let type_chars: Vec<char> = record.type_string.chars().collect();
    let mut rendered_rows = Vec::with_capacity(rows.len());
    for row in &rows {
        if row.len() != type_chars.len() {
            return Outcome::Fail(format!(
                "row has {} columns, type string {:?} declares {}",
                row.len(),
                record.type_string,
                type_chars.len()
            ));
        }
        rendered_rows.push(
            row.iter()
                .zip(&type_chars)
                .map(|(v, t)| sqllogictest_value(v, *t))
                .collect::<Vec<_>>(),
        );
    }
    let flat = flatten_sorted(rendered_rows, record.sort_mode);

    match &record.expected {
        Expected::Values(expected) => {
            if &flat == expected {
                Outcome::Pass
            } else {
                Outcome::Fail(format!("expected {expected:?}, got {flat:?}"))
            }
        }
        Expected::Hash { count, digest } => {
            if flat.len() != *count {
                return Outcome::Fail(format!(
                    "expected {count} values hashing to {digest}, got {} values",
                    flat.len()
                ));
            }
            let mut hasher = Md5::new();
            for value in &flat {
                hasher.update(value.as_bytes());
                hasher.update(b"\n");
            }
            let got_digest = to_hex(&hasher.finalize());
            if &got_digest == digest {
                Outcome::Pass
            } else {
                Outcome::Fail(format!(
                    "expected {count} values hashing to {digest}, got hash {got_digest}"
                ))
            }
        }
    }
}

pub fn run_file(oracle: &Path, script_path: &Path) -> FileTally {
    let text = std::fs::read_to_string(script_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", script_path.display()));
    let records = crate::format::parse_script(&text);

    let file_name = script_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| script_path.display().to_string());

    let db_path: PathBuf =
        std::env::temp_dir().join(format!("sqlite-rs-sqllogictest-{file_name}.db"));
    let _ = std::fs::remove_file(&db_path);

    let mut tally = FileTally {
        file: file_name,
        pass: 0,
        skip: 0,
        fail: 0,
        failures: Vec::new(),
    };
    let mut pending_setup = String::new();

    for record in &records {
        match record {
            Record::Statement(stmt) => {
                if stmt.expect_ok {
                    pending_setup.push_str(&stmt.sql);
                    pending_setup.push_str(";\n");
                }
            }
            Record::Query(query) => {
                if !pending_setup.is_empty() {
                    oracle_exec_write(oracle, &db_path, &pending_setup);
                    pending_setup.clear();
                }
                match run_query(&db_path, query) {
                    Outcome::Pass => tally.pass = tally.pass.saturating_add(1),
                    Outcome::Skip => tally.skip = tally.skip.saturating_add(1),
                    Outcome::Fail(reason) => {
                        tally.fail = tally.fail.saturating_add(1);
                        tally.failures.push(format!(
                            "{}:{}: {reason} — {:?}",
                            tally.file, query.line, query.sql
                        ));
                    }
                }
            }
        }
    }

    let _ = std::fs::remove_file(&db_path);
    tally
}
