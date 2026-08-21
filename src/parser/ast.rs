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
//! NATURAL/RIGHT/FULL joins, `USING`, comma-style joins, `ANY`/`ALL`/
//! `SOME` quantified comparisons, and multi-column `IN` do not exist here
//! at all, nor does FOREIGN KEY/REFERENCES (V8). Subqueries in FROM
//! (#257) are `TableRefKind::Subquery`.
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
    /// `UNION ALL` arms (#240) chained after this `Select`'s own
    /// core (`distinct`/`columns`/`from`/`where_clause`/`group_by`/
    /// `having`). `order_by`/`limit` below apply to the whole compound
    /// statement, not to any individual arm — matching SQLite's
    /// grammar, where only the outermost `select-stmt` carries a
    /// trailing ORDER BY/LIMIT.
    pub compound: Vec<CompoundSelect>,
    pub order_by: Vec<OrderingTerm>,
    pub limit: Option<Limit>,
    pub span: Span,
}

/// One `UNION ALL SELECT ...` arm of a compound `SELECT` (#240). Same
/// shape as `Select`'s own core, minus `order_by`/`limit` (see
/// [`Select::compound`]).
#[derive(Debug, Clone, PartialEq)]
pub struct CompoundSelect {
    pub op: CompoundOp,
    pub distinct: Option<Distinctness>,
    pub columns: Vec<ResultColumn>,
    pub from: Option<FromClause>,
    pub where_clause: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub span: Span,
}

/// Only `UnionAll` is implemented (#240); plain `UNION` (dedup) is
/// deferred to V4 Phase 2, and `INTERSECT`/`EXCEPT` remain unsupported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundOp {
    UnionAll,
}

/// `EXPLAIN [QUERY PLAN] select-stmt` (#243) — pulled forward from its
/// original V7 slot (`.openspec/grammar/sqlite.ebnf`'s `explain-stmt`)
/// because the planner's join equality-index-selection work needs EQP
/// output to be observable now. Wraps only a `Select`: the acceptance
/// criterion this exists for ("EXPLAIN QUERY PLAN shows index usage")
/// is about the join planner, not `EXPLAIN`'s general opcode-dump form
/// over every statement kind — that broader form remains future scope.
#[derive(Debug, Clone, PartialEq)]
pub struct Explain {
    pub query_plan: bool,
    pub select: Box<Select>,
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

/// A `FROM`-clause table entry (#237): either a real catalog table by
/// name, or (#257) a parenthesized `select-stmt` materialized at codegen
/// time into an ephemeral table — `SELECT * FROM (SELECT ...) AS sub`.
#[derive(Debug, Clone, PartialEq)]
pub enum TableRefKind {
    Name(String),
    Subquery(Box<Select>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableRef {
    pub kind: TableRefKind,
    pub alias: Option<String>,
    pub span: Span,
}

impl TableRef {
    /// The catalog name to resolve this table against, or `None` for a
    /// subquery (which has no catalog entry — codegen materializes it
    /// instead).
    pub fn name(&self) -> Option<&str> {
        match &self.kind {
            TableRefKind::Name(name) => Some(name),
            TableRefKind::Subquery(_) => None,
        }
    }
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

/// One `[NATURAL] <join_op> <table> [ON <expr> | USING (col, ...)]` step
/// of a [`FromClause`].
#[derive(Debug, Clone, PartialEq)]
pub struct Join {
    pub op: JoinOp,
    pub table: TableRef,
    /// `None` for [`JoinOp::Cross`] with no `ON`/`USING`, for
    /// `natural: true` joins (the matching columns are resolved from
    /// same-named columns in both tables — semantic resolution deferred
    /// to codegen), and for a bare `JOIN`/`INNER JOIN` with no `ON` —
    /// rejected by the parser, since this V4 slice requires an explicit
    /// condition for non-natural INNER/LEFT/RIGHT/FULL.
    pub constraint: Option<JoinConstraint>,
    /// `true` for `NATURAL [INNER|LEFT|RIGHT|FULL] JOIN` (#250).
    /// `NATURAL CROSS JOIN` is rejected by the parser (not legal SQLite
    /// grammar); comma-style joins (`FROM a, b`) are synthesized as
    /// `natural: false` `JoinOp::Cross` joins, per #250's design.
    pub natural: bool,
}

/// `INNER`/plain `JOIN`, `LEFT [OUTER] JOIN`, `CROSS JOIN` (#237), and
/// `RIGHT [OUTER] JOIN`/`FULL [OUTER] JOIN` (#250).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinOp {
    Inner,
    Left,
    Cross,
    Right,
    Full,
}

/// The join's matching condition: `ON <expr>` (#237) or
/// `USING (col, ...)` (#250, at least one column).
#[derive(Debug, Clone, PartialEq)]
pub enum JoinConstraint {
    On(Expr),
    Using(Vec<String>),
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
    /// `(a, b) IN (SELECT x, y FROM ...)` / `... NOT IN (...)` (#251) —
    /// the multi-column form of [`ExprKind::InSubquery`]. `exprs` is the
    /// LHS tuple (arity >= 2); the subquery's own result-column count
    /// must match it, checked at codegen time once the subquery's
    /// projection is known.
    InSubqueryMulti {
        exprs: Vec<Expr>,
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
