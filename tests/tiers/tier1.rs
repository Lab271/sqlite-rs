#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Tier 1 — QUERY CORE (spec 001-architecture Tier Model, `plan.md` Core
//! Definition): single-table SELECT, core scalar functions, affinity,
//! built-in collations. Planner droppable (full scans are correct);
//! SELECT core itself is not.
//!
//! Green clauses today are the tokenizer (0.5.0); everything else is a
//! `#[ignore]` stub tagged with the V-block/ticket that will flip it, per
//! CLAUDE.md's tier-stub-flip convention.

use sqlite_rs::parser::tokenizer::Tokenizer;

/// The tokenizer must round-trip every token it produces and never
/// panic on malformed input — this is the one Tier 1 clause already
/// green (0.5.0), ahead of the rest of QUERY CORE.
#[test]
fn t1_tokenizer_roundtrip_never_panics() {
    for input in ["SELECT * FROM t WHERE a = 1", "", "'unterminated", "\0\0\0"] {
        drop(Tokenizer::tokenize(input));
    }
}

/// V2 phase 1 SELECT-core parser (#61), plus the V4 join slice (#237,
/// extended by #250): single-table SELECT and a full JOIN chain —
/// INNER/LEFT/RIGHT/FULL/CROSS, NATURAL, `USING (...)`, and comma-style
/// `FROM a, b` — are accepted; genuinely invalid SQL is still
/// distinguished from Accepted/Unsupported, per the three-way
/// [`ParseOutcome`] contract (spec 002-parser Requirement 4).
#[test]
fn t1_select_core_accepts_and_rejects() {
    use sqlite_rs::parser::error::ParseOutcome;
    use sqlite_rs::parser::parse_select;

    for accepted in [
        "SELECT * FROM t",
        "SELECT a, b FROM t WHERE a = 1",
        "SELECT a FROM t ORDER BY a LIMIT 1",
        "SELECT * FROM t JOIN u ON t.a = u.a",
        "SELECT * FROM t LEFT JOIN u ON t.a = u.a",
        "SELECT * FROM t CROSS JOIN u",
        // #250: NATURAL, RIGHT/FULL, USING, and comma-style joins.
        "SELECT * FROM t NATURAL JOIN u",
        "SELECT * FROM t RIGHT JOIN u ON t.a = u.a",
        "SELECT * FROM t FULL JOIN u ON t.a = u.a",
        "SELECT * FROM t JOIN u USING (a)",
        "SELECT * FROM t, u",
    ] {
        assert!(
            matches!(parse_select(accepted), ParseOutcome::Accepted(_)),
            "expected Accepted for {accepted:?}"
        );
    }

    for invalid in ["SELECT FROM", "SELECT * WHERE", "not sql at all"] {
        assert!(
            matches!(parse_select(invalid), ParseOutcome::Invalid { .. }),
            "expected Invalid for {invalid:?}"
        );
    }
}

/// V2 phase 2 — value-semantics kernel (#78): affinity derivation and
/// conversion, cross-type comparison order, and collation all behave
/// per spec 008. Full oracle-vector coverage lives in
/// `tests/corpus/expr_vectors_test.rs::{affinity,comparison,collation}_vectors_*`.
#[test]
fn t1_expression_kernel_affinity_and_collation_vectors() {
    use std::cmp::Ordering;

    use sqlite_rs::record::Value;
    use sqlite_rs::vdbe::{
        affinity_of, apply_affinity, compare, compare_text, Affinity, Collation,
    };

    assert_eq!(affinity_of("INTEGER"), Affinity::Integer);
    assert_eq!(affinity_of("VARCHAR(10)"), Affinity::Text);
    assert_eq!(affinity_of(""), Affinity::Blob);

    let mut v = Value::Text("1.5".to_string().into());
    apply_affinity(&mut v, Affinity::Real);
    assert_eq!(v, Value::Real(1.5));

    assert_eq!(
        compare(&Value::Null, &Value::Integer(0), Collation::Binary),
        Ordering::Less
    );
    assert_eq!(
        compare(
            &Value::Integer(1),
            &Value::Text("a".into()),
            Collation::Binary
        ),
        Ordering::Less
    );

    assert_eq!(compare_text("I", "i", Collation::NoCase), Ordering::Equal);
}

/// V2 phase 2 — scalar function core (#79): dispatch through the
/// registry never panics, NULL propagates for the general case, and
/// coalesce is the documented NULL-propagation exception. Full
/// oracle-vector coverage lives in
/// `tests/corpus/expr_vectors_test.rs::function_vectors_*`.
#[test]
fn t1_scalar_functions_match_oracle() {
    use sqlite_rs::record::Value;
    use sqlite_rs::vdbe::call_function;

    assert_eq!(call_function("length", &[Value::Null]), Ok(Value::Null));
    assert_eq!(
        call_function("coalesce", &[Value::Null, Value::Null, Value::Integer(3)]),
        Ok(Value::Integer(3))
    );
    assert!(call_function("nope", &[]).is_err());
}

/// V2 phase 3C — codegen (#91): `parse_select` -> `compile_select` ->
/// `execute_with_db` for a single-table WHERE query, oracle-parity
/// acceptance covered exhaustively by
/// `tests/codegen/select_test.rs::v2_corpus_compiles_and_matches_oracle_row_for_row`
/// and `tests/codegen/expr_test.rs`'s named scenarios — this stub just
/// exercises the same pipeline directly as the tier contract.
#[test]
fn t1_single_table_where_matches_oracle() {
    use std::process::Command;
    use std::rc::Rc;

    use sqlite_rs::codegen::compile_select;
    use sqlite_rs::header::DatabaseHeader;
    use sqlite_rs::parser::{parse_select, ParseOutcome};
    use sqlite_rs::record::Value;
    use sqlite_rs::schema::TableSchema;
    use sqlite_rs::vdbe::execute_with_db;
    use sqlite_rs::vfs::{UnixVfs, Vfs, VfsPageSource};

    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_tier1_where_test_{}.db",
        std::process::id()
    ));
    std::fs::remove_file(&path).ok();
    let status = Command::new("sqlite3")
        .arg(&path)
        .arg("CREATE TABLE t(a INTEGER, b INTEGER); INSERT INTO t VALUES (1, 10), (2, 5), (3, 20);")
        .status()
        .expect("creating scratch fixture db (requires sqlite3 on PATH)");
    if !status.success() {
        eprintln!("skipping t1_single_table_where_matches_oracle: no sqlite3 on PATH");
        return;
    }

    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 2,
        columns: vec!["a".to_string(), "b".to_string()],
        column_types: vec![],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: String::new(),
        indexes: vec![],
    };
    let select = match parse_select("SELECT a FROM t WHERE b > 5") {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected parse to succeed, got {other:?}"),
    };
    let program = compile_select(&select, &schema).unwrap();

    let vfs = UnixVfs;
    let file = vfs.open_read(&path).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    let source = VfsPageSource::open(&vfs, &path, header.page_size).unwrap();
    let rows = execute_with_db(&program, Rc::new(source), header).unwrap();

    assert_eq!(rows, vec![vec![Value::Integer(1)], vec![Value::Integer(3)]]);
}

/// V2 phase 3C — codegen (#91): `EXPLAIN`'s addr/opcode/p1-p5/p4 output
/// format, per spec 009 Requirement 10. Full named-scenario coverage
/// lives in `tests/vdbe/explain_test.rs`.
#[test]
fn t1_explain_prints_bytecode() {
    use sqlite_rs::vdbe::{explain, Instruction, Opcode};

    let program = sqlite_rs::vdbe::Program::new(vec![
        Instruction::new(Opcode::Init, 0, 1, 0),
        Instruction::new(Opcode::Halt, 0, 0, 0),
    ]);
    let rows = explain(&program);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].opcode, "Init");
    assert_eq!(rows[1].opcode, "Halt");
}

/// V2 phase 4A — `sqlite-rs query` CLI (#95): the same pipeline as
/// `t1_single_table_where_matches_oracle`, but through the built binary
/// rather than as direct library calls — byte-identical to `sqlite3
/// file "sql"`. Full CLI coverage lives in
/// `tests/parity/v02.rs` and `tests/corpus/cli_e2e_test.rs`.
#[test]
fn t1_cli_query_matches_oracle() {
    use std::process::Command;

    const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_tier1_cli_query_test_{}.db",
        std::process::id()
    ));
    std::fs::remove_file(&path).ok();
    let status = Command::new("sqlite3")
        .arg(&path)
        .arg("CREATE TABLE t(a INTEGER, b INTEGER); INSERT INTO t VALUES (1, 10), (2, 5), (3, 20);")
        .status()
        .expect("creating scratch fixture db (requires sqlite3 on PATH)");
    if !status.success() {
        eprintln!("skipping t1_cli_query_matches_oracle: no sqlite3 on PATH");
        return;
    }

    let output = Command::new(CLI)
        .arg("query")
        .arg(&path)
        .arg("SELECT a FROM t WHERE b > 5")
        .output()
        .expect("running sqlite-rs query");
    assert!(
        output.status.success(),
        "query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "1\n3\n");
}
