// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! AST for the V2 SELECT-core slice (spike 006, issue #57).
//!
//! Scoped to `.openspec/grammar/sqlite.ebnf`'s `(* V2 *)`-tagged rules only:
//! single-FROM SELECT, WHERE, ORDER BY, LIMIT/OFFSET, and the V2 expression
//! grammar. No CREATE TABLE/INSERT/UPDATE/DELETE (V3) and no GROUP BY/HAVING/
//! joins/subqueries (V4) productions exist here at all -- that absence is the
//! thing this spike is testing.

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    pub distinct: Option<Distinctness>,
    pub columns: Vec<ResultColumn>,
    pub from: Option<String>,
    pub where_clause: Option<Expr>,
    pub order_by: Vec<OrderingTerm>,
    pub limit: Option<Limit>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Distinctness {
    Distinct,
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResultColumn {
    Star,
    Expr { expr: Expr, alias: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderingTerm {
    pub expr: Expr,
    /// `None` = no ASC/DESC given, `Some(true)` = DESC.
    pub desc: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub limit: Expr,
    pub offset: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Literal),
    Column {
        table: Option<String>,
        name: String,
    },
    FunctionCall {
        name: String,
        distinct: bool,
        args: Vec<Expr>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Between {
        expr: Box<Expr>,
        lo: Box<Expr>,
        hi: Box<Expr>,
        negated: bool,
    },
    In {
        expr: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
    },
    Like {
        expr: Box<Expr>,
        pattern: Box<Expr>,
        negated: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Integer(i64),
    Float(f64),
    Str(String),
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Plus,
    Minus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Concat,
}

pub fn binary(op: BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
    Expr::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

pub fn unary(op: UnaryOp, expr: Expr) -> Expr {
    Expr::Unary {
        op,
        expr: Box::new(expr),
    }
}
