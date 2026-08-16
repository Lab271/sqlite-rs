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
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_codegen_select_test_{}.db",
        std::process::id()
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
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: String::new(),
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
/// oracle-exactly yet — see `tests/codegen/expr_test.rs`'s `KNOWN_GAPS`
/// doc comment for the underlying reasons (no bitwise/concat opcode in
/// the frozen V2 set, CAST's lossy-conversion semantics beyond
/// affinity coercion, and REAL-literal representation as text).
const KNOWN_GAPS: &[&str] = &[
    "CAST(name AS REAL)",
    "a = 1.;",
    "a & 1",
    "a | 1",
    "a << 1",
    "a >> 1",
    "~a",
];
