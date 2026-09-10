// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Bare `?` placeholders are numbered in the order they appear in the SQL
//! text, at parse time.
//!
//! This is `sqlite3ExprAssignVarNumber`'s contract, and it is not a
//! stylistic choice. The indices used to be handed out by codegen, from a
//! counter on `RegAlloc`, which produced two silent wrong answers a
//! consumer reported against the embedding API:
//!
//! - `UPDATE t SET v = ? WHERE k = ?` — `compile_update` compiles the
//!   `WHERE` operand before the `SET` assignments, so the two placeholders
//!   were numbered backwards and the bindings arrived swapped. The
//!   statement matched nothing and reported `Ok(0)` — which is also the
//!   optimistic-concurrency "lost the race" signal, so a compare-and-swap
//!   built on it failed every time while looking like a live conflict.
//! - `SELECT <index columns only> FROM t WHERE a = ? AND b = ?` on a table
//!   with a usable index — the covering-index plan compiles its seek keys
//!   through a second `RegAlloc`, whose counter started at zero, so both
//!   placeholders became index 1 and the statement reported wanting one
//!   parameter when it had two.
//!
//! There were eight `RegAlloc::new()` sites, so the numbering depended on
//! both visit order and plan shape. Asserting on the AST rather than on a
//! compiled program is deliberate: it pins the property at the point where
//! it is now decided, so no future plan can reintroduce the divergence.

use sqlite_rs::parser::ast::{Expr, ExprKind, InsertSource, ParamKind};
use sqlite_rs::parser::{parse_delete, parse_insert, parse_select, parse_update, ParseOutcome};

/// Every parameter index in `expr`, in the order the walk finds them.
fn params(expr: &Expr) -> Vec<u32> {
    let mut found = Vec::new();
    walk(expr, &mut found);
    found
}

fn walk(expr: &Expr, out: &mut Vec<u32>) {
    match &expr.kind {
        ExprKind::Param(ParamKind::Anonymous(n) | ParamKind::Numbered(n)) => out.push(*n),
        ExprKind::Binary { lhs, rhs, .. } => {
            walk(lhs, out);
            walk(rhs, out);
        }
        ExprKind::Unary { expr, .. } => walk(expr, out),
        ExprKind::Paren(inner) => walk(inner, out),
        _ => {}
    }
}

fn update(sql: &str) -> sqlite_rs::parser::ast::Update {
    match parse_update(sql) {
        ParseOutcome::Accepted(stmt) => *stmt,
        other => panic!("{sql} did not parse: {other:?}"),
    }
}

/// The reported defect, at the layer that caused it: `SET` is written
/// first, so `SET` takes index 1.
#[test]
fn an_update_numbers_set_before_where() {
    let stmt = update("UPDATE t SET v = ? WHERE k = ?");

    assert_eq!(
        params(&stmt.assignments.first().unwrap().value),
        vec![1],
        "the SET placeholder is written first, so it is parameter 1"
    );
    assert_eq!(
        params(stmt.where_clause.as_ref().unwrap()),
        vec![2],
        "the WHERE placeholder is written second, so it is parameter 2 — \
         even though codegen compiles the WHERE first"
    );
}

/// Several assignments and several WHERE terms, to pin the whole sequence
/// rather than just the two-placeholder case.
#[test]
fn a_wider_update_numbers_strictly_left_to_right() {
    let stmt = update("UPDATE t SET a = ?, b = ?, c = ? WHERE d = ? AND e = ?");

    let set: Vec<u32> = stmt
        .assignments
        .iter()
        .flat_map(|a| params(&a.value))
        .collect();
    assert_eq!(set, vec![1, 2, 3]);
    assert_eq!(params(stmt.where_clause.as_ref().unwrap()), vec![4, 5]);
}

/// `?NNN` raises the high-water mark and a later bare `?` continues past
/// it, which is SQLite's rule. Mixing the forms is legal and this is the
/// case a per-expression counter got wrong in both directions.
#[test]
fn an_explicit_index_raises_the_high_water_mark() {
    let stmt = update("UPDATE t SET a = ?, b = ?7, c = ? WHERE d = ?");

    let set: Vec<u32> = stmt
        .assignments
        .iter()
        .flat_map(|a| params(&a.value))
        .collect();
    assert_eq!(
        set,
        vec![1, 7, 8],
        "a bare ? after ?7 takes 8, not 3 — one past the highest so far"
    );
    assert_eq!(params(stmt.where_clause.as_ref().unwrap()), vec![9]);
}

/// The same `?NNN` twice is one parameter, bound once, and must not
/// advance the counter twice.
#[test]
fn a_repeated_explicit_index_is_one_parameter() {
    let stmt = update("UPDATE t SET a = ?1, b = ?1 WHERE c = ?");

    let set: Vec<u32> = stmt
        .assignments
        .iter()
        .flat_map(|a| params(&a.value))
        .collect();
    assert_eq!(set, vec![1, 1]);
    assert_eq!(params(stmt.where_clause.as_ref().unwrap()), vec![2]);
}

/// The covering-index shape from the second report. The numbering is a
/// parse-time property, so it holds regardless of which plan codegen then
/// picks — which is the whole point of moving it here.
#[test]
fn a_select_numbers_its_where_terms_left_to_right() {
    let ParseOutcome::Accepted(stmt) = parse_select("SELECT a, b FROM t WHERE b = ? AND a = ?")
    else {
        panic!("did not parse");
    };
    assert_eq!(params(stmt.where_clause.as_ref().unwrap()), vec![1, 2]);
}

#[test]
fn an_insert_numbers_its_values_left_to_right() {
    let ParseOutcome::Accepted(stmt) = parse_insert("INSERT INTO t VALUES (?, ?, ?7, ?)") else {
        panic!("did not parse");
    };
    let InsertSource::Values(rows) = &stmt.source else {
        panic!("expected a VALUES source");
    };
    let row = rows.first().expect("one VALUES row");
    let seen: Vec<u32> = row.iter().flat_map(params).collect();
    assert_eq!(seen, vec![1, 2, 7, 8]);
}

#[test]
fn a_delete_numbers_its_where_terms_left_to_right() {
    let ParseOutcome::Accepted(stmt) = parse_delete("DELETE FROM t WHERE a = ? AND b = ?") else {
        panic!("did not parse");
    };
    assert_eq!(params(stmt.where_clause.as_ref().unwrap()), vec![1, 2]);
}

/// Numbering restarts for each statement, so two statements parsed through
/// the same entry point cannot inherit each other's counter.
#[test]
fn numbering_is_per_statement() {
    for _ in 0..2 {
        let stmt = update("UPDATE t SET v = ? WHERE k = ?");
        assert_eq!(params(&stmt.assignments.first().unwrap().value), vec![1]);
        assert_eq!(params(stmt.where_clause.as_ref().unwrap()), vec![2]);
    }
}
