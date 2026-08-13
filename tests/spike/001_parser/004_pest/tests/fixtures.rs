//! Fixture test for the pest spike variant: every statement in ../fixtures/valid.sql
//! must parse, every statement in ../fixtures/invalid.sql must fail (never panic).

use std::time::Instant;

use spike_pest::{parse, Expr, Order, ResultColumn, Stmt};

const VALID: &str = include_str!("../../fixtures/valid.sql");
const INVALID: &str = include_str!("../../fixtures/invalid.sql");

/// Statements are separated by `;` + newline.
fn statements(src: &str) -> Vec<&str> {
    src.split(";\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn every_valid_statement_parses() {
    let stmts = statements(VALID);
    let mut failures = Vec::new();
    for s in &stmts {
        match parse(s) {
            Ok(ast) => {
                // Sanity-check that the AST is actually populated, not an empty shell.
                match ast {
                    Stmt::CreateTable { ref columns, .. } => assert!(!columns.is_empty()),
                    Stmt::Insert { ref rows, .. } => assert!(!rows.is_empty()),
                    Stmt::Select(ref sel) => assert!(!sel.columns.is_empty()),
                    Stmt::Update { ref sets, .. } => assert!(!sets.is_empty()),
                    Stmt::Delete { .. } => {}
                }
            }
            Err(e) => failures.push(format!("{s}\n{e}")),
        }
    }
    println!(
        "valid.sql: {}/{} parsed",
        stmts.len() - failures.len(),
        stmts.len()
    );
    assert!(
        failures.is_empty(),
        "{} valid statement(s) failed to parse:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn every_invalid_statement_is_rejected() {
    let stmts = statements(INVALID);
    let mut accepted = Vec::new();
    for s in &stmts {
        // `parse` must return Err, and must not panic.
        if let Ok(ast) = parse(s) {
            accepted.push(format!("{s}  =>  {ast:?}"));
        }
    }
    println!(
        "invalid.sql: {}/{} rejected",
        stmts.len() - accepted.len(),
        stmts.len()
    );
    assert!(
        accepted.is_empty(),
        "{} invalid statement(s) were accepted:\n{}",
        accepted.len(),
        accepted.join("\n")
    );
}

/// Spot-check that precedence follows the shared ladder, not left-to-right.
#[test]
fn precedence_matches_ebnf_ladder() {
    let Ok(Stmt::Select(sel)) = parse("SELECT a FROM t WHERE 1 + 2 * 3 = 7") else {
        panic!("expected a select");
    };
    // `=` is lowest here, then `+`, then `*`.
    let dump = format!("{:?}", sel.filter.unwrap());
    assert!(dump.starts_with(r#"Binary { op: "=""#), "{dump}");
    assert!(dump.contains(r#"Binary { op: "+""#), "{dump}");
    assert!(dump.contains(r#"Binary { op: "*""#), "{dump}");

    // NOT binds looser than `=`: NOT (b = 2), and OR looser than both.
    let Ok(Stmt::Select(sel)) = parse("SELECT a FROM t WHERE a = -1 OR NOT b = 2") else {
        panic!("expected a select");
    };
    let dump = format!("{:?}", sel.filter.unwrap());
    assert!(dump.starts_with(r#"Binary { op: "OR""#), "{dump}");
    assert!(dump.contains(r#"Unary { op: "NOT", expr: Binary { op: "="#), "{dump}");
}

/// Proof that the `Pairs` -> AST conversion carries every clause through.
#[test]
fn ast_carries_every_clause() {
    let sql = "SELECT a AS x, count(DISTINCT b) FROM t WHERE a > 1 \
               GROUP BY a HAVING a > 1 ORDER BY a, b DESC LIMIT 10 OFFSET 5";
    let Ok(Stmt::Select(sel)) = parse(sql) else {
        panic!("expected a select");
    };
    assert_eq!(sel.columns.len(), 2);
    assert_eq!(
        sel.columns[0],
        ResultColumn::Expr {
            expr: Expr::Column { table: None, name: "a".into() },
            alias: Some("x".into()),
        }
    );
    assert_eq!(
        sel.columns[1],
        ResultColumn::Expr {
            expr: Expr::Func {
                name: "count".into(),
                distinct: true,
                args: vec![Expr::Column { table: None, name: "b".into() }],
            },
            alias: None,
        }
    );
    assert_eq!(sel.from.as_deref(), Some("t"));
    assert!(sel.filter.is_some());
    assert_eq!(sel.group_by.len(), 1);
    assert!(sel.having.is_some());
    assert_eq!(
        sel.order_by.iter().map(|(_, d)| *d).collect::<Vec<_>>(),
        vec![Order::Asc, Order::Desc]
    );
    assert_eq!(sel.limit, Some(Expr::Number("10".into())));
    assert_eq!(sel.offset, Some(Expr::Number("5".into())));

    // CREATE TABLE column metadata.
    let Ok(Stmt::CreateTable { table, if_not_exists, columns }) =
        parse("CREATE TABLE IF NOT EXISTS u (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
    else {
        panic!("expected a create table");
    };
    assert_eq!((table.as_str(), if_not_exists), ("u", true));
    assert_eq!(columns[0].type_name.as_deref(), Some("INTEGER"));
    assert!(columns[0].primary_key && !columns[0].not_null);
    assert!(columns[1].not_null && !columns[1].primary_key);
}

/// Prints a real parse error (for the toolchain error-quality comparison) and a
/// rough throughput number. Run with `cargo test -- --nocapture`.
#[test]
fn error_message_and_timing() {
    let err = parse("SELECT * FROM t WHERE a = ").unwrap_err();
    println!("--- example parse error ---\n{err}\n---------------------------");

    let stmts = statements(VALID);
    const ITERS: usize = 1000;
    let start = Instant::now();
    for _ in 0..ITERS {
        for s in &stmts {
            parse(s).unwrap();
        }
    }
    let elapsed = start.elapsed();
    println!(
        "parsed {} statements x {ITERS} in {:?} ({:.1} us per full valid.sql pass, {:.2} us per statement)",
        stmts.len(),
        elapsed,
        elapsed.as_secs_f64() * 1e6 / ITERS as f64,
        elapsed.as_secs_f64() * 1e6 / (ITERS * stmts.len()) as f64,
    );
}
