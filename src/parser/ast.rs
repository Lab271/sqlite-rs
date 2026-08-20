//! AST for the V2 SELECT-core slice plus the V3 DML/DDL slice (spec
//! 002-parser Requirements 2-4), plus the V4 join slice (#237), the V4
//! subquery-expression slice (#238), and the V4 GROUP BY/HAVING slice
//! (#239).
//!
//! Scoped to `.openspec/grammar/sqlite.ebnf`'s `(* V2 *)`/`(* V3 *)`/
//! `(* V4 *)`-tagged rules: SELECT with an INNER/LEFT [OUTER]/CROSS join
//! chain (`FromClause`/`Join`/`JoinOp`/`JoinConstraint`, #237), WHERE,
//! GROUP BY, HAVING (#239), ORDER BY, LIMIT/OFFSET, the V2 expression
//! grammar, INSERT/UPDATE/DELETE, and CREATE/DROP TABLE/INDEX, plus the
//! V4 subquery-expression slice (#238, including correlated
//! subqueries): scalar subqueries (`ExprKind::Subquery`), `IN (SELECT
//! ...)` (`ExprKind::InSubquery`), and `EXISTS (SELECT ...)`
//! (`ExprKind::Exists`) — correlation is resolved at codegen time
//! (`Scope::with_outer`), not represented differently in the AST.
//! NATURAL/RIGHT/FULL joins, `USING`, comma-style joins, subqueries in
//! FROM, `ANY`/`ALL`/`SOME` quantified comparisons, and multi-column
//! `IN` do not exist here at all, nor does FOREIGN KEY/REFERENCES (V8).
//!
//! Every node carries a [`Span`] (Requirement 3: "AST completeness") and
//! parenthesized expressions are preserved explicitly via `ExprKind::Paren`
//! rather than discarded, so `SELECT (a + b) * c` round-trips its grouping.

use super::tokenizer::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub or_action: Option<ConflictAction>,
    pub table: String,
    pub assignments: Vec<Assignment>,
    pub where_clause: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    /// One column for `col = expr`; the tuple form
    /// `(col1, col2) = (expr1, expr2)` is expanded into one [`Assignment`]
    /// per column, each carrying its paired expr from the RHS list.
    pub columns: Vec<String>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    pub distinct: Option<Distinctness>,
    pub columns: Vec<ResultColumn>,
    pub from: Option<FromClause>,
    pub where_clause: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
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

/// A `FROM` clause (#237): the first table plus zero or more joins,
/// evaluated left-to-right — `a JOIN b ON .. JOIN c ON ..` joins `b`
/// against `a`, then `c` against that result. Bare `Option<TableRef>`
/// (V2 scope) was replaced by this once a second table entered scope;
/// the single-table case is simply `joins: vec![]`.
#[derive(Debug, Clone, PartialEq)]
pub struct FromClause {
    pub first: TableRef,
    pub joins: Vec<Join>,
}

/// One `<join_op> <table> [ON <expr>]` step of a [`FromClause`].
#[derive(Debug, Clone, PartialEq)]
pub struct Join {
    pub op: JoinOp,
    pub table: TableRef,
    /// `None` only for [`JoinOp::Cross`] (and a bare `JOIN`/`INNER JOIN`
    /// with no `ON` — rejected by the parser, since this V4 slice
    /// requires an explicit condition for INNER/LEFT).
    pub constraint: Option<JoinConstraint>,
}

/// `INNER`/plain `JOIN`, `LEFT [OUTER] JOIN`, and `CROSS JOIN` — the V4
/// slice (#237). `NATURAL`/`RIGHT`/`FULL` and comma-style joins are still
/// parse-time `unsupported(..)` errors (see `grammar.rs::from_clause`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinOp {
    Inner,
    Left,
    Cross,
}

/// The join's matching condition. `USING (...)` is out of scope for this
/// slice — only `ON <expr>` is represented.
#[derive(Debug, Clone, PartialEq)]
pub enum JoinConstraint {
    On(Expr),
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
    /// A scalar subquery `(SELECT ...)` (#238) — usable anywhere an
    /// expression is, including correlated (a reference to an enclosing
    /// query's column).
    Subquery(Box<Select>),
    /// `EXISTS (SELECT ...)` / `NOT EXISTS (SELECT ...)` (#238).
    Exists {
        subquery: Box<Select>,
        negated: bool,
    },
    /// `expr IN (SELECT ...)` / `expr NOT IN (SELECT ...)` (#238) — kept
    /// separate from [`ExprKind::In`]'s literal-list form rather than a
    /// union, so callers pattern-matching on `In` don't need to handle a
    /// subquery case.
    InSubquery {
        expr: Box<Expr>,
        subquery: Box<Select>,
        negated: bool,
    },
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

#[derive(Debug, Clone, PartialEq)]
pub struct CreateTable {
    pub if_not_exists: bool,
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<TableConstraint>,
    pub without_rowid: bool,
    pub strict: bool,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub type_name: Option<String>,
    pub constraints: Vec<ColumnConstraint>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ColumnConstraint {
    NotNull,
    PrimaryKey {
        /// `None` = no ASC/DESC given, `Some(true)` = DESC.
        desc: Option<bool>,
        autoincrement: bool,
    },
    Unique,
    Default(DefaultValue),
    Check(Expr),
    Collate(String),
}

/// `DEFAULT` accepts either a bare literal or a parenthesized expression
/// (never a bare non-literal expression) — kept as separate variants so
/// the printer knows which form reparses correctly.
#[derive(Debug, Clone, PartialEq)]
pub enum DefaultValue {
    Literal(Expr),
    Paren(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TableConstraint {
    PrimaryKey(Vec<IndexedColumn>),
    Unique(Vec<IndexedColumn>),
    Check(Expr),
}

/// An indexed-column: an expression (bare column ref, `COLLATE`-qualified,
/// or a general expression for functional indexes), plus optional
/// ASC/DESC. Shared by `CREATE INDEX` and `PRIMARY KEY`/`UNIQUE` table
/// constraints — unlike [`OrderingTerm`], NULLS FIRST/LAST doesn't apply.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexedColumn {
    pub expr: Expr,
    pub desc: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateIndex {
    pub unique: bool,
    pub if_not_exists: bool,
    pub name: String,
    pub table: String,
    pub columns: Vec<IndexedColumn>,
    pub where_clause: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DropTable {
    pub if_exists: bool,
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DropIndex {
    pub if_exists: bool,
    pub name: String,
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
