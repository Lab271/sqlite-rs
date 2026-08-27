// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Minimal AST for the spike grammar subset (tests/spike/001_parser/grammar/sqlite-subset.ebnf).
//!
//! Deliberately smaller than the sketch in `.openspec/specs/002-parser/spec.md`: just
//! enough structure to prove that pomelo's semantic actions can build a real tree.

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    CreateTable {
        name: String,
        if_not_exists: bool,
        columns: Vec<ColumnDef>,
    },
    Insert {
        table: String,
        columns: Vec<String>,
        rows: Vec<Vec<Expr>>,
    },
    Select(Select),
    Update {
        table: String,
        assignments: Vec<(String, Expr)>,
        where_clause: Option<Expr>,
    },
    Delete {
        table: String,
        where_clause: Option<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub type_name: Option<String>,
    pub constraints: Vec<ColConstraint>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ColConstraint {
    NotNull,
    PrimaryKey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    pub distinct: Option<Distinctness>,
    pub columns: Vec<ResultColumn>,
    pub from: Option<String>,
    pub where_clause: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
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

/// Helpers used by the grammar's semantic actions.
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
