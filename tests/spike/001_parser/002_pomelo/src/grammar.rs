// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! pomelo (Lemon-as-a-proc-macro) grammar for the spike SQL subset.
//!
//! Mirrors `tests/spike/001_parser/grammar/sqlite-subset.ebnf`. As in SQLite's own
//! `parse.y`, the expression grammar is a single flat `expr` non-terminal and all
//! ambiguity is resolved by the precedence declarations below rather than by a
//! stratified rule ladder.

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

    // NOTE: pomelo 0.2.3 short-circuits to %parse_fail when the error is hit at
    // end-of-input (`yymajor == 0`), *without* calling %syntax_error -- so for a
    // truncated statement we get no offending token and no expected-token list.
    %parse_fail { "syntax error: unexpected end of input (incomplete statement)".to_string() }
    %stack_overflow { "parser stack overflow".to_string() }

    %token #[derive(Debug, Clone, PartialEq)] pub enum Token {};

    // ---- terminals carrying data ----
    %type Id String;
    %type Integer i64;
    %type Float f64;
    %type Str String;

    // ---- precedence, LOWEST first; mirrors parse.y:295-309 ----
    %left Or;
    %left And;
    %right Not;
    %left Eq Ne Lt Le Gt Ge;
    %left Plus Minus;
    %left Star Slash Rem;
    %left Concat;
    // precedence-only terminal, standing in for parse.y's BITNOT slot which gives
    // unary +/- their (highest) precedence.
    %right Unary;

    // ===================== Top level =====================
    %type stmt Stmt;
    stmt ::= create_table(S) { S }
    stmt ::= insert(S) { S }
    stmt ::= select(S) { S }
    stmt ::= update(S) { S }
    stmt ::= delete(S) { S }

    // ===================== CREATE TABLE =====================
    %type create_table Stmt;
    create_table ::= Create Table if_not_exists(E) Id(N) LParen column_defs(C) RParen {
        Stmt::CreateTable { name: N, if_not_exists: E, columns: C }
    }

    %type if_not_exists bool;
    if_not_exists ::= { false }
    if_not_exists ::= If Not Exists { true }

    %type column_defs Vec<ColumnDef>;
    column_defs ::= column_def(C) { vec![C] }
    column_defs ::= column_defs(mut L) Comma column_def(C) { L.push(C); L }

    %type column_def ColumnDef;
    column_def ::= Id(N) type_name_opt(T) col_constraints(C) {
        ColumnDef { name: N, type_name: T, constraints: C }
    }

    %type type_name_opt Option<String>;
    type_name_opt ::= { None }
    type_name_opt ::= type_name(T) { Some(T) }

    %type type_name String;
    type_name ::= Id(A) { A }
    type_name ::= type_name(mut A) Id(B) { A.push(' '); A.push_str(&B); A }

    %type col_constraints Vec<ColConstraint>;
    col_constraints ::= { Vec::new() }
    col_constraints ::= col_constraints(mut L) col_constraint(C) { L.push(C); L }

    %type col_constraint ColConstraint;
    col_constraint ::= Not Null { ColConstraint::NotNull }
    col_constraint ::= Primary Key { ColConstraint::PrimaryKey }

    // ===================== INSERT =====================
    %type insert Stmt;
    insert ::= Insert Into Id(T) insert_cols(C) Values row_list(R) {
        Stmt::Insert { table: T, columns: C, rows: R }
    }

    %type insert_cols Vec<String>;
    insert_cols ::= { Vec::new() }
    insert_cols ::= LParen id_list(L) RParen { L }

    %type id_list Vec<String>;
    id_list ::= Id(A) { vec![A] }
    id_list ::= id_list(mut L) Comma Id(A) { L.push(A); L }

    %type row_list Vec<Vec<Expr>>;
    row_list ::= LParen expr_list(E) RParen { vec![E] }
    row_list ::= row_list(mut L) Comma LParen expr_list(E) RParen { L.push(E); L }

    // ===================== SELECT =====================
    %type select Stmt;
    select ::= Select distinct(D) result_columns(C) from_opt(F) where_opt(W)
               group_opt(G) order_opt(O) limit_opt(L) {
        let (group_by, having) = G;
        Stmt::Select(Select {
            distinct: D, columns: C, from: F, where_clause: W,
            group_by, having, order_by: O, limit: L,
        })
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

    %type group_opt (Vec<Expr>, Option<Expr>);
    group_opt ::= { (Vec::new(), None) }
    group_opt ::= Group By expr_list(G) { (G, None) }
    group_opt ::= Group By expr_list(G) Having expr(H) { (G, Some(H)) }

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

    // ===================== UPDATE =====================
    %type update Stmt;
    update ::= Update Id(T) Set set_list(S) where_opt(W) {
        Stmt::Update { table: T, assignments: S, where_clause: W }
    }

    %type set_list Vec<(String, Expr)>;
    set_list ::= set_item(I) { vec![I] }
    set_list ::= set_list(mut L) Comma set_item(I) { L.push(I); L }

    %type set_item (String, Expr);
    set_item ::= Id(N) Eq expr(E) { (N, E) }

    // ===================== DELETE =====================
    %type delete Stmt;
    delete ::= Delete From Id(T) where_opt(W) {
        Stmt::Delete { table: T, where_clause: W }
    }

    // ===================== Expressions =====================
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

    %type expr_list Vec<Expr>;
    expr_list ::= expr(E) { vec![E] }
    expr_list ::= expr_list(mut L) Comma expr(E) { L.push(E); L }
}

pub use sql::{Parser, Token};
