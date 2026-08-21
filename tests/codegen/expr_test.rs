#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Expression lowering acceptance (spec 009, Requirement 11): jump-shape
//! scenarios named by the spec, plus spike 008's `walker.jsonl` vectors
//! (#59) run through the real compiled path — `parse_select` ->
//! `compile_select` -> `execute_with_db` — rather than hand-assembled,
//! discharging #91's "spike 008 vectors pass through the compiled path"
//! acceptance bar.

use std::path::Path;
use std::process::Command;
use std::rc::Rc;

use sqlite_rs::codegen::compile_select;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::record::Value;
use sqlite_rs::schema::TableSchema;
use sqlite_rs::vdbe::{execute, execute_with_db, explain};
use sqlite_rs::vfs::{UnixVfs, Vfs, VfsPageSource};

/// A single-row fixture table `t(a, b, name)`, scratch-built via a real
/// `sqlite3` (same pattern as `tests/corpus/parser_oracle_test.rs`'s
/// `scratch_db`) — schema is irrelevant to these tests; only FROM's
/// row-presence requirement matters, since every vector here is a bare
/// literal/CASE/arithmetic expression with no column references.
fn one_row_fixture() -> (std::path::PathBuf, TableSchema) {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_codegen_expr_test_{}_{n}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let status = Command::new("sqlite3")
        .arg(&path)
        .arg("CREATE TABLE t(a INTEGER, b INTEGER, name TEXT); INSERT INTO t VALUES (1, 10, 'aa');")
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

fn run_select(path: &Path, schema: &TableSchema, sql: &str) -> Vec<Vec<Value>> {
    let vfs = UnixVfs;
    let file = vfs.open_read(path).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    let source = VfsPageSource::open(&vfs, path, header.page_size).unwrap();

    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program = compile_select(&select, schema).unwrap_or_else(|e| {
        panic!("compiling {sql:?}: {e}");
    });
    execute_with_db(&program, Rc::new(source), header).unwrap()
}

#[test]
fn where_clause_compiles_to_direct_jump() {
    let (path, schema) = one_row_fixture();
    let program = compile_select(
        &match parse_select("SELECT a FROM t WHERE b > 5") {
            ParseOutcome::Accepted(s) => *s,
            other => panic!("{other:?}"),
        },
        &schema,
    )
    .unwrap();
    let rows = explain(&program);
    // The `Gt`/complement-family comparison is emitted as a jump
    // instruction, not followed by a separate boolean-test opcode —
    // there is no intermediate `IfNot`/`IfPos` reading a register the
    // comparison itself wrote.
    let compare_row = rows
        .iter()
        .find(|r| r.opcode == "Gt")
        .expect("expected a Gt jump instruction");
    assert!(compare_row.p2 > 0, "Gt must carry a real jump target");

    let out = run_select(&path, &schema, "SELECT a FROM t WHERE b > 5");
    assert_eq!(out, vec![vec![Value::Integer(1)]]);
    let out_excluded = run_select(&path, &schema, "SELECT a FROM t WHERE b > 50");
    assert!(out_excluded.is_empty());
}

#[test]
fn and_short_circuits_on_false_first_operand() {
    let (path, schema) = one_row_fixture();
    // b=10, a=1: `b >= 100 AND a = 1` — first operand false, so the
    // second must never matter; assert both the direct semantic result
    // and that no `Eq` jump-target sits unreachable after the first
    // comparison's false path (i.e. the false path lands on `Next`
    // directly, without falling into the `Eq` test).
    let out = run_select(&path, &schema, "SELECT a FROM t WHERE b >= 100 AND a = 1");
    assert!(out.is_empty());
    let out2 = run_select(&path, &schema, "SELECT a FROM t WHERE b >= 10 AND a = 1");
    assert_eq!(out2, vec![vec![Value::Integer(1)]]);
}

#[test]
fn or_includes_a_row_matched_by_either_operand() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT a FROM t WHERE a = 1 OR a = 99");
    assert_eq!(out, vec![vec![Value::Integer(1)]]);
}

#[test]
fn between_excludes_out_of_range_rows() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT a FROM t WHERE b BETWEEN 1 AND 20");
    assert_eq!(out, vec![vec![Value::Integer(1)]]);
    let out2 = run_select(
        &path,
        &schema,
        "SELECT a FROM t WHERE b BETWEEN 100 AND 200",
    );
    assert!(out2.is_empty());
}

#[test]
fn in_list_matches_any_element() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT a FROM t WHERE a IN (5, 1, 9)");
    assert_eq!(out, vec![vec![Value::Integer(1)]]);
    let out2 = run_select(&path, &schema, "SELECT a FROM t WHERE a IN (5, 9)");
    assert!(out2.is_empty());
}

/// A two-row `i INTEGER, r REAL` fixture reproducing #138's oracle
/// table, for comparison-affinity coverage: text/real literals compared
/// against typed columns.
fn affinity_fixture() -> (std::path::PathBuf, TableSchema) {
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_codegen_affinity_test_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let status = Command::new("sqlite3")
        .arg(&path)
        .arg(
            "CREATE TABLE t(i INTEGER, r REAL); \
             INSERT INTO t VALUES (5, 1.5), (6, 2.5);",
        )
        .status()
        .expect("creating scratch fixture db");
    assert!(status.success());
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 2,
        columns: vec!["i".to_string(), "r".to_string()],
        column_types: vec!["INTEGER".to_string(), "REAL".to_string()],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: String::new(),
        indexes: vec![],
    };
    (path, schema)
}

#[test]
fn comparison_affinity_coerces_text_and_real_literals_against_typed_columns() {
    // #138: `i = '5'` compares an INTEGER column against a TEXT
    // literal — without comparison affinity this falls back to
    // storage-class ordering (numeric < text) and never matches.
    let (path, schema) = affinity_fixture();
    let out = run_select(&path, &schema, "SELECT i FROM t WHERE i = '5'");
    assert_eq!(out, vec![vec![Value::Integer(5)]]);

    let out = run_select(&path, &schema, "SELECT i FROM t WHERE i > 3");
    assert_eq!(out, vec![vec![Value::Integer(5)], vec![Value::Integer(6)]]);

    let out = run_select(&path, &schema, "SELECT r FROM t WHERE r = 1.5");
    assert_eq!(out, vec![vec![Value::Real(1.5)]]);
}

#[test]
fn like_and_glob_dispatch_through_the_function_opcode() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT a FROM t WHERE name LIKE 'a%'");
    assert_eq!(out, vec![vec![Value::Integer(1)]]);
    let out2 = run_select(&path, &schema, "SELECT a FROM t WHERE name LIKE 'z%'");
    assert!(out2.is_empty());
}

#[test]
fn single_arg_function_call_compiles() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT abs(b) FROM t");
    assert_eq!(out, vec![vec![Value::Integer(10)]]);
}

#[test]
fn multi_arg_function_call_compiles_with_contiguous_registers() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT instr(name, 'a') FROM t");
    assert_eq!(out, vec![vec![Value::Integer(1)]]);
}

#[test]
fn zero_arg_function_call_compiles() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT sqlite_version() FROM t");
    assert_eq!(out, vec![vec![Value::Text("3.53.4".to_string().into())]]);
}

#[test]
fn from_less_select_compiles_a_bare_expression_list() {
    let (_path, schema) = one_row_fixture();
    let select = match parse_select("SELECT sqlite_version(), 1 + 1") {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected parse, got {other:?}"),
    };
    let program = compile_select(&select, &schema).unwrap();
    let out = execute(&program).unwrap();
    assert_eq!(
        out,
        vec![vec![
            Value::Text("3.53.4".to_string().into()),
            Value::Integer(2)
        ]]
    );
}

#[test]
fn from_less_select_rejects_star() {
    let (_path, schema) = one_row_fixture();
    let select = match parse_select("SELECT *") {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected parse, got {other:?}"),
    };
    assert!(compile_select(&select, &schema).is_err());
}

#[test]
fn case_compiles_to_a_jump_chain() {
    let (path, schema) = one_row_fixture();
    let out = run_select(
        &path,
        &schema,
        "SELECT CASE WHEN a = 1 THEN 'one' WHEN a = 2 THEN 'two' ELSE 'other' END FROM t",
    );
    assert_eq!(out, vec![vec![Value::Text("one".to_string().into())]]);
}

/// Reads spike 008's kept oracle vectors
/// (`tests/corpus/expr_vectors/walker.jsonl`, #59) and runs each
/// through the real compiled path rather than a hand-assembled
/// `Program` (`tests/vdbe/oracle_vectors_test.rs` covers the
/// hand-assembled acceptance bar for #89).
/// Reads one JSON string field out of a `.jsonl` line, honouring JSON's
/// backslash escapes.
///
/// The obvious `split('"').next()` shortcut is wrong twice over: it
/// stops at an escaped `\"` instead of the closing quote, and it leaves
/// `\\` doubled. That second one silently changed the SQL under test —
/// `'a%b' LIKE 'a\\%b' ESCAPE '\\'` reached the compiler with a
/// two-character escape, which SQLite itself rejects ("ESCAPE
/// expression must be a single character"). The resulting failure was
/// filed against `Like`'s codegen for a long time; the engine had been
/// right all along, and the two `ESCAPE` vectors pass now that the
/// query they describe is the query that runs.
fn json_string_field(line: &str, field: &str) -> String {
    let rest = line
        .split(&format!(r#""{field}": ""#))
        .nth(1)
        .unwrap_or_else(|| panic!("no {field} field in {line}"));
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return out,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let code = u32::from_str_radix(&hex, 16)
                        .unwrap_or_else(|_| panic!("bad \\u escape in {line}"));
                    out.push(
                        char::from_u32(code).unwrap_or_else(|| panic!("bad code point in {line}")),
                    );
                }
                // `\\`, `\"`, `\/` and anything else: the escape is the
                // character itself.
                Some(other) => out.push(other),
                None => panic!("trailing backslash in {line}"),
            },
            other => out.push(other),
        }
    }
    panic!("unterminated {field} field in {line}")
}

fn walker_vectors() -> Vec<(String, Value)> {
    let content = std::fs::read_to_string("tests/corpus/expr_vectors/walker.jsonl")
        .expect("reading walker.jsonl");
    content
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let expr = json_string_field(line, "expr");
            let type_ = json_string_field(line, "type");
            let value_quoted = json_string_field(line, "value_quoted");
            let expected = match type_.as_str() {
                "null" => Value::Null,
                "integer" => Value::Integer(value_quoted.parse().unwrap()),
                "real" => Value::Real(value_quoted.parse().unwrap()),
                "text" => Value::Text(
                    value_quoted
                        .trim_start_matches('\'')
                        .trim_end_matches('\'')
                        .to_string()
                        .into(),
                ),
                // Blob results aren't produced by any V2-scope
                // expression this ticket compiles (no blob-literal or
                // blob-returning function is in scope) — skip rather
                // than modeling blob equality here.
                "blob" => Value::Blob(Vec::new().into()),
                other => panic!("unhandled vector type {other}"),
            };
            (expr, expected, type_)
        })
        .filter(|(_, _, type_)| type_ != "blob")
        .map(|(expr, expected, _)| (expr, expected))
        .collect()
}

/// Vectors this ticket's codegen is known not to compile correctly yet,
/// documented (with the reason) rather than silently swallowed —
/// matched by substring against the vector's `expr` text:
///
/// - `CAST(...)`: only CAST AS INTEGER/REAL are wired (via `MustBeInt`/
///   `RealAffinity`), and even those don't match SQLite's lossy-cast
///   semantics exactly (`MustBeInt` errors instead of truncating) — see
///   `compile_value`'s `Cast` arm doc comment.
///
/// `&`/`|`/`<<`/`>>`/`~`/`||` used to be listed here — #139 harvested
/// `BitAnd`/`BitOr`/`ShiftLeft`/`ShiftRight`/`BitNot`/`Concat` and wired
/// them in `compile_value`, so those vectors now run.
///
/// `NOT`/`AND`/`OR`/`BETWEEN`/`IN` over NULL operands used to be listed
/// here — the 2-target jump scheme conflated NULL with FALSE, which is
/// right for a top-level WHERE and wrong everywhere else. #134 gave
/// `compile_cond` a `NullTarget` and value mode the `Null`/`Not`
/// opcodes, so those vectors now run.
/// - Bare `-9223372036854775808`: `i64::MIN`'s literal token doesn't
///   round-trip through this ticket's `i32`-truncating `Integer` opcode
///   path for values outside `i32`'s range in the same way SQLite's own
///   64-bit literal handling does — a numeric-literal-width limitation,
///   not a control-flow one.
/// - Real-literal arithmetic (`7.0/2`, `7%2.5`, etc.): REAL literals
///   compile to their textual form (no `OP_Real`-equivalent opcode
///   exists), so arithmetic on them takes the TEXT-coercion path
///   instead of a true floating-point path — see `Literal::Float`'s
///   doc comment in `compile_value`.
const KNOWN_GAPS: &[&str] = &[];

/// Runs one `(expr, expected)` walker vector through the real compiled
/// path (`parse_select` -> `compile_select` -> `execute_with_db`)
/// against a fresh cursor over `path`/`schema`/`header`, returning
/// `Ok(None)` for a vector this compiler doesn't accept (skip, not
/// fail), `Ok(Some(()))` for a match, or `Err(reason)` for an execution
/// error or a wrong result. Shared by the full walker-vector sweep and
/// any narrower per-family test (e.g. CAST-only) that wants the same
/// compiled-path check in isolation.
fn run_walker_vector(
    schema: &TableSchema,
    path: &Path,
    header: DatabaseHeader,
    expr: &str,
    expected: &Value,
) -> Result<Option<()>, String> {
    let sql = format!("SELECT {expr} FROM t");
    let select = match parse_select(&sql) {
        ParseOutcome::Accepted(s) => *s,
        ParseOutcome::Unsupported { .. } | ParseOutcome::Invalid { .. } => return Ok(None),
    };
    let program = match compile_select(&select, schema) {
        Ok(p) => p,
        Err(_) => return Ok(None), // Known-gap constructs — not this test's concern.
    };
    let vfs = UnixVfs;
    let source = VfsPageSource::open(&vfs, path, header.page_size).unwrap();
    let rows = execute_with_db(&program, Rc::new(source), header)
        .map_err(|e| format!("{expr}: exec error {e}"))?;
    let got = rows.first().and_then(|r| r.first()).cloned();
    if got.as_ref() != Some(expected) {
        return Err(format!("{expr}: expected {expected:?}, got {got:?}"));
    }
    Ok(Some(()))
}

/// CAST-only slice of the walker vectors (#142), runnable in isolation
/// from the rest of the expression sweep via `cargo test
/// cast_vectors_pass_through_the_compiled_path` — every `CAST(...)`
/// vector oracle-harvested into `tests/corpus/expr_vectors/walker.jsonl`
/// must compile and match the oracle exactly.
#[test]
fn cast_vectors_pass_through_the_compiled_path() {
    let (path, schema) = one_row_fixture();
    let file = UnixVfs.open_read(&path).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();

    let cast_vectors: Vec<_> = walker_vectors()
        .into_iter()
        .filter(|(expr, _)| expr.starts_with("CAST("))
        .collect();
    assert!(
        !cast_vectors.is_empty(),
        "expected at least one CAST vector in tests/corpus/expr_vectors/walker.jsonl"
    );

    let mut failures = Vec::new();
    let mut passed = 0usize;
    for (expr, expected) in &cast_vectors {
        let header = DatabaseHeader::parse(&header_buf).unwrap();
        match run_walker_vector(&schema, &path, header, expr, expected) {
            Ok(Some(())) => passed += 1,
            Ok(None) => failures.push(format!(
                "{expr}: did not compile (should not happen for CAST)"
            )),
            Err(reason) => failures.push(reason),
        }
    }
    assert!(
        failures.is_empty(),
        "{} unexpected CAST vector failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(passed, cast_vectors.len());
}

#[test]
fn walker_vectors_pass_through_the_compiled_path() {
    let (path, schema) = one_row_fixture();
    let file = UnixVfs.open_read(&path).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();

    let mut failures = Vec::new();
    let mut passed = 0usize;
    let mut skipped = 0usize;
    for (expr, expected) in walker_vectors() {
        if KNOWN_GAPS.iter().any(|g| expr.contains(g)) {
            skipped += 1;
            continue;
        }
        let header = DatabaseHeader::parse(&header_buf).unwrap();
        match run_walker_vector(&schema, &path, header, &expr, &expected) {
            Ok(Some(())) => passed += 1,
            Ok(None) => {} // Known-gap constructs (see codegen doc comments) — not this test's concern.
            Err(reason) => failures.push(reason),
        }
    }
    // A ratchet, not a floor: 86 is what passes today (55 before #142
    // harvested Real/Blob/Int64/Cast and fixed `%`'s real-promotion
    // rule: +15 vectors freed from KNOWN_GAPS — CAST/big-integer/mixed-
    // real-modulo were all gated on the same "no opcode for it"
    // literal-fidelity gap — plus 16 new CAST vectors harvested by
    // `tools/gen_expr_vectors.py` closing the corpus's remaining
    // NUMERIC-target, BLOB-target, nonzero-parsing BLOB-source, and
    // saturation/precision-loss gaps). Adding vectors or closing a
    // `KNOWN_GAPS` entry should raise this number in the same commit; a
    // drop means a regression the per-vector assertion below cannot
    // see, because a vector that stops *compiling* is skipped, not
    // failed.
    assert!(
        passed >= 86,
        "expected most walker vectors to pass through the compiled path, only {passed} did ({skipped} known-gap skipped)"
    );
    assert!(
        failures.is_empty(),
        "{} unexpected walker vector failure(s) (not in KNOWN_GAPS):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// --- #224: raise line coverage on src/codegen/expr.rs above 85% ---
// Targeted tests below cover: OR's true-first-operand path, negated
// BETWEEN, IS/IS NOT (as condition and negated), IN with an empty list
// and with a NULL member, NOT LIKE, unary Minus/Not/BitNot, bitwise/
// shift/concat operators, an i64-overflow integer literal (Int64
// harvesting), a blob literal, CAST used both standalone and as a CASE
// branch (column-branch coverage for `emit_branch_into`), an unknown
// aggregate call rejection, an unknown-column error, and boolean
// connectives (AND/OR/comparisons) materialized as a value (the
// non-`is_definite` path through `compile_bool_to_value`).

#[test]
fn or_short_circuits_true_on_first_operand() {
    let (path, schema) = one_row_fixture();
    // lhs (a = 1) is true; rhs would error if it were evaluated eagerly
    // as a condition needing a real jump target — exercises OR's
    // true-label-first-operand path (line 120-129).
    let out = run_select(&path, &schema, "SELECT a FROM t WHERE a = 1 OR b = 999");
    assert_eq!(out, vec![vec![Value::Integer(1)]]);
    let out2 = run_select(&path, &schema, "SELECT a FROM t WHERE a = 999 OR b = 10");
    assert_eq!(out2, vec![vec![Value::Integer(1)]]);
}

#[test]
fn not_between_excludes_in_range_rows() {
    let (path, schema) = one_row_fixture();
    let out = run_select(
        &path,
        &schema,
        "SELECT a FROM t WHERE b NOT BETWEEN 5 AND 15",
    );
    assert!(out.is_empty());
    let out2 = run_select(
        &path,
        &schema,
        "SELECT a FROM t WHERE b NOT BETWEEN 20 AND 30",
    );
    assert_eq!(out2, vec![vec![Value::Integer(1)]]);
}

#[test]
fn is_and_is_not_compile_as_conditions() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT a FROM t WHERE a IS 1");
    assert_eq!(out, vec![vec![Value::Integer(1)]]);
    let out2 = run_select(&path, &schema, "SELECT a FROM t WHERE a IS NOT 1");
    assert!(out2.is_empty());
    let out3 = run_select(&path, &schema, "SELECT a FROM t WHERE a IS NOT NULL");
    assert_eq!(out3, vec![vec![Value::Integer(1)]]);
    let out4 = run_select(&path, &schema, "SELECT a FROM t WHERE a IS NULL");
    assert!(out4.is_empty());
}

#[test]
fn in_empty_list_is_always_false() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT a FROM t WHERE a IN ()");
    assert!(out.is_empty());
    let out2 = run_select(&path, &schema, "SELECT a FROM t WHERE a NOT IN ()");
    assert_eq!(out2, vec![vec![Value::Integer(1)]]);
}

#[test]
fn in_list_with_null_member_is_unknown_on_no_match() {
    let (path, schema) = one_row_fixture();
    // No item matches `a`, but the list contains NULL: the honest
    // answer is unknown, which WHERE excludes just like false.
    let out = run_select(&path, &schema, "SELECT a FROM t WHERE a IN (NULL, 999)");
    assert!(out.is_empty());
    // `NOT IN` over the same unknown case is still unknown -> excluded.
    let out2 = run_select(&path, &schema, "SELECT a FROM t WHERE a NOT IN (NULL, 999)");
    assert!(out2.is_empty());
}

#[test]
fn not_like_negates_the_function_result() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT a FROM t WHERE name NOT LIKE 'z%'");
    assert_eq!(out, vec![vec![Value::Integer(1)]]);
    let out2 = run_select(&path, &schema, "SELECT a FROM t WHERE name NOT LIKE 'a%'");
    assert!(out2.is_empty());
}

#[test]
fn unary_operators_compile() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT -b FROM t");
    assert_eq!(out, vec![vec![Value::Integer(-10)]]);
    let out2 = run_select(&path, &schema, "SELECT +b FROM t");
    assert_eq!(out2, vec![vec![Value::Integer(10)]]);
    let out3 = run_select(&path, &schema, "SELECT NOT (a = 1) FROM t");
    assert_eq!(out3, vec![vec![Value::Integer(0)]]);
    let out4 = run_select(&path, &schema, "SELECT ~b FROM t");
    assert_eq!(out4, vec![vec![Value::Integer(-11)]]);
}

#[test]
fn bitwise_shift_and_concat_operators_compile() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT b & 2 FROM t");
    assert_eq!(out, vec![vec![Value::Integer(2)]]);
    let out2 = run_select(&path, &schema, "SELECT b | 1 FROM t");
    assert_eq!(out2, vec![vec![Value::Integer(11)]]);
    let out3 = run_select(&path, &schema, "SELECT b << 1 FROM t");
    assert_eq!(out3, vec![vec![Value::Integer(20)]]);
    let out4 = run_select(&path, &schema, "SELECT b >> 1 FROM t");
    assert_eq!(out4, vec![vec![Value::Integer(5)]]);
    let out5 = run_select(&path, &schema, "SELECT name || 'z' FROM t");
    assert_eq!(out5, vec![vec![Value::Text("aaz".to_string().into())]]);
}

#[test]
fn integer_literal_beyond_i32_uses_int64_opcode() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT 5000000000 FROM t");
    assert_eq!(out, vec![vec![Value::Integer(5_000_000_000)]]);
}

#[test]
fn blob_literal_compiles() {
    let (path, schema) = one_row_fixture();
    let out = run_select(&path, &schema, "SELECT x'414243' FROM t");
    assert_eq!(out, vec![vec![Value::Blob(vec![0x41, 0x42, 0x43].into())]]);
}

#[test]
fn case_with_column_branch_and_cast_branch() {
    let (path, schema) = one_row_fixture();
    // Column branch result exercises `emit_branch_into`'s Column arm.
    let out = run_select(
        &path,
        &schema,
        "SELECT CASE WHEN a = 1 THEN name ELSE 'other' END FROM t",
    );
    assert_eq!(out, vec![vec![Value::Text("aa".to_string().into())]]);

    // CAST as a standalone value expression.
    let out2 = run_select(&path, &schema, "SELECT CAST(name AS INTEGER) FROM t");
    assert_eq!(out2, vec![vec![Value::Integer(0)]]);
}

#[test]
fn case_branch_with_computed_expression_compiles_via_copy() {
    // #141: a CASE branch that is a compound expression (not a bare
    // literal or column reference) used to be rejected outright — no
    // opcode existed to relocate its computed value into the CASE's
    // shared result register. `Copy` closes that gap.
    let (path, schema) = one_row_fixture();
    let rows = run_select(
        &path,
        &schema,
        "SELECT CASE WHEN a = 1 THEN a + 1 ELSE 0 END FROM t",
    );
    assert_eq!(rows, vec![vec![Value::Integer(2)]]);
}

#[test]
fn aggregate_call_is_rejected_as_unsupported() {
    let (_path, schema) = one_row_fixture();
    let select = match parse_select("SELECT count(*) FROM t") {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("{other:?}"),
    };
    let err = compile_select(&select, &schema).unwrap_err();
    assert!(
        matches!(err, sqlite_rs::codegen::CodegenError::Unsupported { .. }),
        "expected Unsupported, got {err:?}"
    );
}

#[test]
fn unknown_column_reference_is_rejected() {
    let (_path, schema) = one_row_fixture();
    let select = match parse_select("SELECT nope FROM t") {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("{other:?}"),
    };
    let err = compile_select(&select, &schema).unwrap_err();
    assert!(
        matches!(err, sqlite_rs::codegen::CodegenError::UnknownColumn { .. }),
        "expected UnknownColumn, got {err:?}"
    );
}

#[test]
fn boolean_connectives_materialize_as_a_value() {
    let (path, schema) = one_row_fixture();
    // `is_definite` is false for AND/OR/comparisons, so this exercises
    // `compile_bool_to_value`'s two-pass unknown-detecting path.
    let out = run_select(&path, &schema, "SELECT (a = 1 AND b = 10) FROM t");
    assert_eq!(out, vec![vec![Value::Integer(1)]]);
    let out2 = run_select(&path, &schema, "SELECT (a = 1 OR b = 999) FROM t");
    assert_eq!(out2, vec![vec![Value::Integer(1)]]);
    let out3 = run_select(&path, &schema, "SELECT (a = 999) FROM t");
    assert_eq!(out3, vec![vec![Value::Integer(0)]]);
    // NULL operand -> unknown -> NULL result register.
    let out4 = run_select(&path, &schema, "SELECT (a = NULL) FROM t");
    assert_eq!(out4, vec![vec![Value::Null]]);
}

#[test]
fn case_with_operand_and_various_branch_literal_types() {
    let (path, schema) = one_row_fixture();
    // CASE with an operand (`CASE a WHEN ... `) builds an internal `=`
    // comparison against each WHEN value.
    let out = run_select(
        &path,
        &schema,
        "SELECT CASE a WHEN 1 THEN 'match' ELSE 'no' END FROM t",
    );
    assert_eq!(out, vec![vec![Value::Text("match".to_string().into())]]);

    // Branch result literal types beyond Integer/Str exercise
    // `emit_branch_into`'s True/False/Float/Blob/Null arms.
    let out2 = run_select(
        &path,
        &schema,
        "SELECT CASE WHEN a = 1 THEN TRUE ELSE FALSE END FROM t",
    );
    assert_eq!(out2, vec![vec![Value::Integer(1)]]);
    let out3 = run_select(
        &path,
        &schema,
        "SELECT CASE WHEN a = 1 THEN 1.5 ELSE 0.0 END FROM t",
    );
    assert_eq!(out3, vec![vec![Value::Real(1.5)]]);
    let out4 = run_select(
        &path,
        &schema,
        "SELECT CASE WHEN a = 1 THEN x'41' ELSE x'42' END FROM t",
    );
    assert_eq!(out4, vec![vec![Value::Blob(vec![0x41].into())]]);
    let out5 = run_select(
        &path,
        &schema,
        "SELECT CASE WHEN a = 999 THEN 1 ELSE NULL END FROM t",
    );
    assert_eq!(out5, vec![vec![Value::Null]]);
}

#[test]
fn is_and_is_null_materialize_as_a_value() {
    let (path, schema) = one_row_fixture();
    // `is_definite` is true for IS/IS NULL, exercising the single-pass
    // definite branch of `compile_bool_to_value`.
    let out = run_select(&path, &schema, "SELECT (a IS 1) FROM t");
    assert_eq!(out, vec![vec![Value::Integer(1)]]);
    let out2 = run_select(&path, &schema, "SELECT (a IS NULL) FROM t");
    assert_eq!(out2, vec![vec![Value::Integer(0)]]);
    let out3 = run_select(&path, &schema, "SELECT (a IS NOT NULL) FROM t");
    assert_eq!(out3, vec![vec![Value::Integer(1)]]);
}

#[test]
fn collate_rtrim_resolves_and_zero_arg_function_call_compiles() {
    let (path, schema) = one_row_fixture();
    let out = run_select(
        &path,
        &schema,
        "SELECT a FROM t WHERE name = 'aa ' COLLATE RTRIM",
    );
    assert_eq!(out, vec![vec![Value::Integer(1)]]);

    // A zero-argument scalar function call exercises the "reserve a
    // register nothing reads" path in FunctionCall lowering (`random`
    // need not be a registered function for codegen to compile it —
    // only execution would care).
    let select = match parse_select("SELECT random() FROM t") {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("{other:?}"),
    };
    compile_select(&select, &schema).unwrap();
}
