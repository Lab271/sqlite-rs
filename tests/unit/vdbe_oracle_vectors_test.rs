// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Acceptance oracle for spec 009 (#89): hand-assembled `Program`s that
//! reproduce spike 008's expression vectors (`tests/corpus/expr_vectors/`)
//! bit-exact through the VDBE opcodes this ticket implements — arithmetic,
//! compare, and NULL propagation. Full expression coverage is codegen's
//! job (#91, Req-11); this test demonstrates the acceptance oracle
//! itself: the same vectors that gated spike 008's kernel now gate the
//! opcodes that call it.

use std::fs;

use sqlite_rs::record::Value;
use sqlite_rs::vdbe::{execute, Collation, Instruction, Opcode, Program, P4};

/// Looks up a vector by its exact `expr` field in one of spike 008's
/// oracle corpora, returning `(type, value_quoted)`.
fn vector(file: &str, expr: &str) -> (String, String) {
    let path = format!("tests/corpus/expr_vectors/{file}");
    let content = fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    for line in content.lines().filter(|l| !l.is_empty()) {
        let want = format!(r#""expr": "{expr}""#);
        if line.contains(&want) {
            let type_ = line
                .split(r#""type": ""#)
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or_else(|| panic!("no type field in {line}"))
                .to_string();
            let value_quoted = line
                .split(r#""value_quoted": ""#)
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or_else(|| panic!("no value_quoted field in {line}"))
                .to_string();
            return (type_, value_quoted);
        }
    }
    panic!("no vector with expr {expr:?} found in {path}");
}

fn expect_value(type_: &str, value_quoted: &str) -> Value {
    match type_ {
        "null" => Value::Null,
        "integer" => Value::Integer(value_quoted.parse().unwrap()),
        "real" => Value::Real(value_quoted.parse().unwrap()),
        "text" => Value::Text(value_quoted.to_string().into()),
        other => panic!("unhandled vector type {other}"),
    }
}

#[test]
fn coercion_vectors_reproduce_through_hand_assembled_add() {
    // Each case: (jsonl expr, text literal, integer addend).
    let cases = [
        ("'123' + 1", "123"),
        ("'123abc' + 1", "123abc"),
        ("'abc' + 1", "abc"),
        ("'  123  ' + 1", "  123  "),
    ];
    for (expr, text_literal) in cases {
        let (type_, value_quoted) = vector("coercion.jsonl", expr);
        let expected = expect_value(&type_, &value_quoted);

        // String8 text_literal -> r0; Integer 1 -> r1; Add r0,r1 -> r2;
        // ResultRow r2,1; Halt.
        let program = Program::new(vec![
            Instruction::with_p4(Opcode::String8, 0, 0, 0, P4::Str(text_literal.to_string())),
            Instruction::new(Opcode::Integer, 1, 1, 0),
            Instruction::new(Opcode::Add, 0, 1, 2),
            Instruction::new(Opcode::ResultRow, 2, 1, 0),
            Instruction::new(Opcode::Halt, 0, 0, 0),
        ]);
        let rows = execute(&program).unwrap();
        assert_eq!(rows, vec![vec![expected]], "expr {expr:?}");
    }
}

#[test]
fn real_addend_coercion_vector_reproduces_through_hand_assembled_add() {
    let (type_, value_quoted) = vector("coercion.jsonl", "'1e3' + 1");
    let expected = expect_value(&type_, &value_quoted);

    let program = Program::new(vec![
        Instruction::with_p4(Opcode::String8, 0, 0, 0, P4::Str("1e3".to_string())),
        Instruction::new(Opcode::Integer, 1, 1, 0),
        Instruction::new(Opcode::Add, 0, 1, 2),
        Instruction::new(Opcode::ResultRow, 2, 1, 0),
        Instruction::new(Opcode::Halt, 0, 0, 0),
    ]);
    let rows = execute(&program).unwrap();
    assert_eq!(rows, vec![vec![expected]]);
}

#[test]
fn null_propagation_vector_reproduces_through_hand_assembled_add() {
    let (type_, value_quoted) = vector("null.jsonl", "NULL + 1");
    let expected = expect_value(&type_, &value_quoted);
    assert_eq!(expected, Value::Null);

    // r0 is never written, so it reads as NULL (Requirement 2: unwritten
    // registers read as NULL, no implicit-clearing surprises to model).
    let program = Program::new(vec![
        Instruction::new(Opcode::Integer, 1, 1, 0),
        Instruction::new(Opcode::Add, 0, 1, 2),
        Instruction::new(Opcode::ResultRow, 2, 1, 0),
        Instruction::new(Opcode::Halt, 0, 0, 0),
    ]);
    let rows = execute(&program).unwrap();
    assert_eq!(rows, vec![vec![expected]]);
}

#[test]
fn cross_type_comparison_vector_reproduces_through_hand_assembled_lt() {
    // "1 < 'a'" is true: numeric sorts below text in spec 008's
    // cross-type order (Requirement 2 there), independent of the
    // literal values compared.
    let (type_, value_quoted) = vector("comparison.jsonl", "1 < 'a'");
    let expected = expect_value(&type_, &value_quoted);
    assert_eq!(expected, Value::Integer(1));

    // Integer 1 -> r0; String8 'a' -> r1; Integer 0 -> r2 (default,
    // "false"); Lt r0,jump=skip,r1; Integer 1 -> r2 (overwritten if the
    // comparison held); ResultRow r2,1; Halt.
    let program = Program::new(vec![
        Instruction::new(Opcode::Integer, 1, 0, 0),
        Instruction::with_p4(Opcode::String8, 0, 1, 0, P4::Str("a".to_string())),
        Instruction::new(Opcode::Integer, 0, 2, 0),
        Instruction::with_p4(
            Opcode::Lt,
            0,
            5,
            1,
            P4::CollSeq {
                collation: Collation::Binary,
                affinity: 0,
            },
        ),
        Instruction::new(Opcode::Goto, 0, 6, 0),
        Instruction::new(Opcode::Integer, 1, 2, 0),
        Instruction::new(Opcode::ResultRow, 2, 1, 0),
        Instruction::new(Opcode::Halt, 0, 0, 0),
    ]);
    let rows = execute(&program).unwrap();
    assert_eq!(rows, vec![vec![expected]]);
}

#[test]
fn null_comparison_vector_takes_no_jump_through_hand_assembled_lt() {
    // "NULL < 1" is NULL (unknown), not true or false: the Lt opcode
    // must not jump when either operand is NULL (Requirement 5 — no
    // re-derived comparison rule, delegates NULL handling to the kernel
    // by simply declining to jump on an unknown result).
    let (type_, value_quoted) = vector("comparison.jsonl", "NULL < 1");
    assert_eq!(expect_value(&type_, &value_quoted), Value::Null);

    // r0 left NULL; Integer 1 -> r1; Integer 0 -> r2 (default result);
    // Lt r0,jump=skip,r1 (must not jump); Integer 1 -> r2 would only run
    // if the jump were (wrongly) taken.
    let program = Program::new(vec![
        Instruction::new(Opcode::Integer, 1, 1, 0),
        Instruction::new(Opcode::Integer, 0, 2, 0),
        Instruction::new(Opcode::Lt, 0, 99, 1),
        Instruction::new(Opcode::ResultRow, 2, 1, 0),
        Instruction::new(Opcode::Halt, 0, 0, 0),
    ]);
    let rows = execute(&program).unwrap();
    assert_eq!(rows, vec![vec![Value::Integer(0)]]);
}
