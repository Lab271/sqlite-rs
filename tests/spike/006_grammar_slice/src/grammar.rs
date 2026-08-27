// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! pomelo grammar for the V2 SELECT-core *slice* (spike 006, issue #57).
//!
//! Derived from `.openspec/grammar/sqlite.ebnf`'s `(* V2 *)`-tagged rules, and
//! from `tests/spike/001_parser/002_pomelo/src/grammar.rs` (the toolchain spike
//! 001 chose -- see `tests/spike/001_parser/comparison.md`). Unlike that spike,
//! this grammar contains *only* `select` and its expression tree: no
//! CREATE TABLE/INSERT/UPDATE/DELETE (V3) and no GROUP BY/HAVING (V4)
//! productions exist at all. That is the slice under test.
//!
//! BETWEEN/IN/LIKE are included (beyond 002_pomelo's expr grammar) because
//! their `NOT`-prefixed forms are exactly the kind of LALR slicing trap the
//! spike issue calls out: they interact with the `Not` unary-prefix rule and
//! sit at a distinct precedence tier from AND/OR despite BETWEEN's own clause
//! also using the `And` keyword.

use pomelo::pomelo;

pomelo! {
    %module sql;

    %include {
        use crate::ast::*;
    }

    %error String;

    %syntax_error {
        let mut exps: Vec<&str> = expected.map(|e| e.name).collect();
        exps.sort_unstable();
        let truncated = exps.len() > 8;
        exps.truncate(8);
        let mut list = exps.join(", ");
        if truncated {
            list.push_str(", ...");
        }
        Err(match token {
            Some(t) => format!("syntax error: unexpected {:?}; expected one of [{}]", t, list),
            None => format!("syntax error: unexpected end of input; expected one of [{}]", list),
        })
    }

    %parse_fail { "syntax error: unexpected end of input (incomplete statement)".to_string() }
    %stack_overflow { "parser stack overflow".to_string() }

    %token #[derive(Debug, Clone, PartialEq)] pub enum Token {};

    %type Id String;
    %type Integer i64;
    %type Float f64;
    %type Str String;

    // ---- precedence, LOWEST first; mirrors parse.y:295-309 ----
    %left Or;
    %left And;
    %right Not;
    %left Eq Ne Lt Le Gt Ge;
    %nonassoc Between In Like;
    %left Plus Minus;
    %left Star Slash Rem;
    %left Concat;
    %right Unary;

    // ===================== Top level =====================
    // Slice under test: `stmt` has exactly one alternative. Growing V3 back in
    // means *adding* alternatives here, never editing this one (see
    // FINDINGS.md's growth-path probe).
    %type stmt Select;
    stmt ::= select(S) { S }

    // ===================== SELECT (V2 slice) =====================
    %type select Select;
    select ::= Select distinct(D) result_columns(C) from_opt(F) where_opt(W)
               order_opt(O) limit_opt(L) {
        Select {
            distinct: D, columns: C, from: F, where_clause: W,
            order_by: O, limit: L,
        }
    }

    %type distinct Option<Distinctness>;
    distinct ::= { None }
    distinct ::= Distinct { Some(Distinctness::Distinct) }
    distinct ::= All { Some(Distinctness::All) }

    %type result_columns Vec<ResultColumn>;
    result_columns ::= result_column(C) { vec![C] }
    result_columns ::= result_columns(mut L) Comma result_column(C) { L.push(C); L }

    %type result_column ResultColumn;
    result_column ::= Star { ResultColumn::Star }
    result_column ::= expr(E) { ResultColumn::Expr { expr: E, alias: None } }
    result_column ::= expr(E) As Id(A) { ResultColumn::Expr { expr: E, alias: Some(A) } }
    result_column ::= expr(E) Id(A) { ResultColumn::Expr { expr: E, alias: Some(A) } }

    %type from_opt Option<String>;
    from_opt ::= { None }
    from_opt ::= From Id(T) { Some(T) }

    %type where_opt Option<Expr>;
    where_opt ::= { None }
    where_opt ::= Where expr(E) { Some(E) }

    %type order_opt Vec<OrderingTerm>;
    order_opt ::= { Vec::new() }
    order_opt ::= Order By sort_list(S) { S }

    %type sort_list Vec<OrderingTerm>;
    sort_list ::= ordering_term(T) { vec![T] }
    sort_list ::= sort_list(mut L) Comma ordering_term(T) { L.push(T); L }

    %type ordering_term OrderingTerm;
    ordering_term ::= expr(E) { OrderingTerm { expr: E, desc: None } }
    ordering_term ::= expr(E) Asc { OrderingTerm { expr: E, desc: Some(false) } }
    ordering_term ::= expr(E) Desc { OrderingTerm { expr: E, desc: Some(true) } }

    %type limit_opt Option<Limit>;
    limit_opt ::= { None }
    limit_opt ::= Limit expr(L) { Some(Limit { limit: L, offset: None }) }
    limit_opt ::= Limit expr(L) Offset expr(O) { Some(Limit { limit: L, offset: Some(O) }) }
    limit_opt ::= Limit expr(L) Comma expr(O) { Some(Limit { limit: L, offset: Some(O) }) }

    // ===================== Expressions (V2 slice) =====================
    %type expr Expr;
    expr ::= expr(A) Or expr(B)      { binary(BinaryOp::Or, A, B) }
    expr ::= expr(A) And expr(B)     { binary(BinaryOp::And, A, B) }
    expr ::= Not expr(A)             { unary(UnaryOp::Not, A) }
    expr ::= expr(A) Eq expr(B)      { binary(BinaryOp::Eq, A, B) }
    expr ::= expr(A) Ne expr(B)      { binary(BinaryOp::Ne, A, B) }
    expr ::= expr(A) Lt expr(B)      { binary(BinaryOp::Lt, A, B) }
    expr ::= expr(A) Le expr(B)      { binary(BinaryOp::Le, A, B) }
    expr ::= expr(A) Gt expr(B)      { binary(BinaryOp::Gt, A, B) }
    expr ::= expr(A) Ge expr(B)      { binary(BinaryOp::Ge, A, B) }

    // BETWEEN/IN/LIKE: the LALR-slicing-trap candidates. BETWEEN's own clause
    // uses the `And` token, but at a *different* precedence tier than the
    // top-level `expr And expr` rule -- both alternatives share a token
    // without sharing a nonterminal.
    expr ::= expr(A) Between expr(B) And expr(C) [Between] {
        Expr::Between { expr: Box::new(A), lo: Box::new(B), hi: Box::new(C), negated: false }
    }
    expr ::= expr(A) Not Between expr(B) And expr(C) [Between] {
        Expr::Between { expr: Box::new(A), lo: Box::new(B), hi: Box::new(C), negated: true }
    }
    expr ::= expr(A) In LParen expr_list(L) RParen [In] {
        Expr::In { expr: Box::new(A), list: L, negated: false }
    }
    expr ::= expr(A) Not In LParen expr_list(L) RParen [In] {
        Expr::In { expr: Box::new(A), list: L, negated: true }
    }
    expr ::= expr(A) Like expr(B) [Like] {
        Expr::Like { expr: Box::new(A), pattern: Box::new(B), negated: false }
    }
    expr ::= expr(A) Not Like expr(B) [Like] {
        Expr::Like { expr: Box::new(A), pattern: Box::new(B), negated: true }
    }

    expr ::= expr(A) Plus expr(B)    { binary(BinaryOp::Add, A, B) }
    expr ::= expr(A) Minus expr(B)   { binary(BinaryOp::Sub, A, B) }
    expr ::= expr(A) Star expr(B)    { binary(BinaryOp::Mul, A, B) }
    expr ::= expr(A) Slash expr(B)   { binary(BinaryOp::Div, A, B) }
    expr ::= expr(A) Rem expr(B)     { binary(BinaryOp::Mod, A, B) }
    expr ::= expr(A) Concat expr(B)  { binary(BinaryOp::Concat, A, B) }
    expr ::= Minus expr(A) [Unary]   { unary(UnaryOp::Minus, A) }
    expr ::= Plus expr(A) [Unary]    { unary(UnaryOp::Plus, A) }
    expr ::= LParen expr(A) RParen   { A }
    expr ::= Integer(N)              { Expr::Literal(Literal::Integer(N)) }
    expr ::= Float(N)                { Expr::Literal(Literal::Float(N)) }
    expr ::= Str(S)                  { Expr::Literal(Literal::Str(S)) }
    expr ::= Null                    { Expr::Literal(Literal::Null) }
    expr ::= Id(N)                   { Expr::Column { table: None, name: N } }
    expr ::= Id(T) Dot Id(N)         { Expr::Column { table: Some(T), name: N } }
    expr ::= Id(F) LParen func_args(A) RParen {
        let (distinct, args) = A;
        Expr::FunctionCall { name: F, distinct, args }
    }

    %type func_args (bool, Vec<Expr>);
    func_args ::= { (false, Vec::new()) }
    func_args ::= expr_list(L) { (false, L) }
    func_args ::= Distinct { (true, Vec::new()) }
    func_args ::= Distinct expr_list(L) { (true, L) }
    func_args ::= Star { (false, vec![Expr::Literal(Literal::Null)]) }

    %type expr_list Vec<Expr>;
    expr_list ::= expr(E) { vec![E] }
    expr_list ::= expr_list(mut L) Comma expr(E) { L.push(E); L }
}

pub use sql::{Parser, Token};
