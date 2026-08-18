//! AST for the V2 SELECT-core slice (spec 002-parser Requirements 2-4).
//!
//! Scoped to `.openspec/grammar/sqlite.ebnf`'s `(* V2 *)`-tagged rules:
//! single-FROM SELECT, WHERE, ORDER BY, LIMIT/OFFSET, and the V2 expression
//! grammar. No CREATE TABLE/INSERT/UPDATE/DELETE (V3) and no GROUP BY/HAVING/
//! joins/subqueries (V4) productions exist here at all.
//!
//! Every node carries a [`Span`] (Requirement 3: "AST completeness") and
//! parenthesized expressions are preserved explicitly via `ExprKind::Paren`
//! rather than discarded, so `SELECT (a + b) * c` round-trips its grouping.

use super::tokenizer::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    pub distinct: Option<Distinctness>,
    pub columns: Vec<ResultColumn>,
    pub from: Option<TableRef>,
    pub where_clause: Option<Expr>,
    pub order_by: Vec<OrderingTerm>,
    pub limit: Option<Limit>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distinctness {
    Distinct,
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResultColumn {
    Star,
    TableStar { table: String },
    Expr { expr: Expr, alias: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableRef {
    pub name: String,
    pub alias: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderingTerm {
    pub expr: Expr,
    /// `None` = no ASC/DESC given, `Some(true)` = DESC.
    pub desc: Option<bool>,
    /// `None` = no NULLS FIRST/LAST given, `Some(true)` = NULLS LAST.
    pub nulls_last: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub limit: Expr,
    pub offset: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Literal(Literal),
    Param(ParamKind),
    Column {
        table: Option<String>,
        catalog: Option<String>,
        name: String,
    },
    FunctionCall {
        name: String,
        distinct: bool,
        args: FunctionArgs,
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
    Is {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        negated: bool,
    },
    IsNull {
        expr: Box<Expr>,
        negated: bool,
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
        glob: bool,
        negated: bool,
        escape: Option<Box<Expr>>,
    },
    Case {
        operand: Option<Box<Expr>>,
        whens: Vec<(Expr, Expr)>,
        else_: Option<Box<Expr>>,
    },
    Cast {
        expr: Box<Expr>,
        type_name: String,
    },
    Collate {
        expr: Box<Expr>,
        collation: String,
    },
    /// A parenthesized expression, preserved explicitly (Requirement 3's
    /// "preserve parentheses for precedence" scenario).
    Paren(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FunctionArgs {
    /// `f(*)`.
    Star,
    List(Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Integer(i64),
    Float(f64),
    Str(String),
    Blob(Vec<u8>),
    Null,
    True,
    False,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamKind {
    /// Bare `?`.
    Anonymous,
    /// `?NNN`.
    Numbered(u32),
    /// `:name`.
    Colon(String),
    /// `@name`.
    At(String),
    /// `$name`.
    Dollar(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Plus,
    Minus,
    BitNot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Insert {
    pub or_action: Option<ConflictAction>,
    pub table: String,
    pub columns: Option<Vec<String>>,
    pub source: InsertSource,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAction {
    Replace,
    Ignore,
    Abort,
    Rollback,
    Fail,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InsertSource {
    Values(Vec<Vec<Expr>>),
    Select(Box<Select>),
    DefaultValues,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Delete {
    pub table: String,
    pub where_clause: Option<Expr>,
    pub span: Span,
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
    BitAnd,
    BitOr,
    Shl,
    Shr,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Concat,
}
