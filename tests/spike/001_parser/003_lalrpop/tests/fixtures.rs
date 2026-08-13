use std::time::Instant;

use spike_lalrpop::{parse, split_statements};

const VALID: &str = include_str!("../../fixtures/valid.sql");
const INVALID: &str = include_str!("../../fixtures/invalid.sql");

#[test]
fn valid_fixtures_all_parse() {
    let stmts = split_statements(VALID);
    assert!(!stmts.is_empty());
    let mut failures = Vec::new();
    for s in &stmts {
        if let Err(e) = parse(s) {
            failures.push(format!("  {s}\n    -> {e}"));
        }
    }
    println!("valid.sql: {} statements, {} failed", stmts.len(), failures.len());
    assert!(failures.is_empty(), "expected all to parse:\n{}", failures.join("\n"));
}

#[test]
fn invalid_fixtures_all_reject() {
    let stmts = split_statements(INVALID);
    assert!(!stmts.is_empty());
    let mut accepted = Vec::new();
    for s in &stmts {
        match parse(s) {
            Ok(ast) => accepted.push(format!("  {s}\n    -> {ast:?}")),
            Err(e) => println!("rejected: {s}\n    -> {e}"),
        }
    }
    println!("invalid.sql: {} statements, {} wrongly accepted", stmts.len(), accepted.len());
    assert!(accepted.is_empty(), "expected all to fail:\n{}", accepted.join("\n"));
}

/// Sanity-check that semantic actions build the tree with the EBNF's
/// precedence ladder (`1 + 2 * 3` == `1 + (2 * 3)`, `NOT a = b` == `NOT (a = b)`).
#[test]
fn precedence_ladder() {
    use spike_lalrpop::ast::*;

    let Stmt::Select(s) = parse("SELECT a FROM t WHERE 1 + 2 * 3 = 7").unwrap() else {
        panic!("expected SELECT")
    };
    let Some(Expr::Binary(lhs, BinaryOp::Eq, _)) = s.where_ else {
        panic!("top of WHERE should be `=`")
    };
    let Expr::Binary(_, BinaryOp::Add, rhs) = *lhs else {
        panic!("lhs of `=` should be `+`")
    };
    assert!(matches!(*rhs, Expr::Binary(_, BinaryOp::Mul, _)), "`*` must bind tighter than `+`");

    let Stmt::Select(s) = parse("SELECT a FROM t WHERE a = -1 OR NOT b = 2").unwrap() else {
        panic!("expected SELECT")
    };
    let Some(Expr::Binary(_, BinaryOp::Or, rhs)) = s.where_ else { panic!("top should be OR") };
    let Expr::Unary(UnaryOp::Not, inner) = *rhs else { panic!("rhs should be NOT") };
    assert!(matches!(*inner, Expr::Binary(_, BinaryOp::Eq, _)), "NOT must wrap the comparison");
}

/// Rough throughput: parse every statement in valid.sql, 1000 times.
#[test]
fn perf_smoke() {
    let stmts = split_statements(VALID);
    const ITERS: u32 = 1000;
    let start = Instant::now();
    for _ in 0..ITERS {
        for s in &stmts {
            let _ = parse(s).unwrap();
        }
    }
    let total = start.elapsed();
    println!(
        "perf: {ITERS} x {} statements = {} parses in {:?} ({:.2} us/stmt, {:?}/file-pass)",
        stmts.len(),
        ITERS as usize * stmts.len(),
        total,
        total.as_secs_f64() * 1e6 / (ITERS as f64 * stmts.len() as f64),
        total / ITERS,
    );
}
