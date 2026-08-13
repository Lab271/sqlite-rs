//! Minimal AST for the spike grammar subset (see
//! `tests/spike/001_parser/grammar/sqlite-subset.ebnf`).
//!
//! Deliberately small: just enough structure to prove that Lemon semantic
//! actions can build a real tree. It is *not* the AST from
//! `.openspec/specs/002-parser/spec.md`.

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    CreateTable {
        if_not_exists: bool,
        name: String,
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
        sets: Vec<(String, Expr)>,
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
    pub not_null: bool,
    pub primary_key: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    /// `Some(true)` = DISTINCT, `Some(false)` = ALL, `None` = unspecified.
    pub distinct: Option<bool>,
    pub columns: Vec<ResultColumn>,
    pub from: Option<String>,
    pub where_clause: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    /// `(expr, descending)`
    pub order_by: Vec<(Expr, bool)>,
    /// `(limit, offset)`
    pub limit: Option<(Expr, Option<Expr>)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResultColumn {
    Star,
    Expr(Expr, Option<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Lit),
    Column {
        table: Option<String>,
        name: String,
    },
    Func {
        name: String,
        distinct: bool,
        args: Vec<Expr>,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Paren(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Int(i64),
    Float(f64),
    Str(String),
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
    Pos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
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
    Rem,
    Concat,
}

/// Strip the surrounding single quotes from a SQL string literal and collapse
/// the doubled-quote escape.
pub fn unquote(text: &str) -> String {
    let inner = text
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(text);
    inner.replace("''", "'")
}
