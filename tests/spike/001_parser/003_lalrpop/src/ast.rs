//! Minimal AST for the parser spike -- just enough to prove that LALRPOP's
//! semantic actions can build a real tree. Not the production AST from
//! `.openspec/specs/002-parser/spec.md`.

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    CreateTable(CreateTable),
    Insert(Insert),
    Select(Select),
    Update(Update),
    Delete(Delete),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateTable {
    pub if_not_exists: bool,
    pub name: String,
    pub columns: Vec<ColumnDef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    /// `type-name ::= identifier { identifier }` -- empty when omitted.
    pub type_name: Vec<String>,
    pub constraints: Vec<ColumnConstraint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnConstraint {
    NotNull,
    PrimaryKey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Insert {
    pub table: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Expr>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    pub distinct: Option<Distinct>,
    pub columns: Vec<ResultColumn>,
    pub from: Option<String>,
    pub where_: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub order_by: Vec<OrderingTerm>,
    pub limit: Option<Limit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distinct {
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
    pub desc: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub limit: Expr,
    pub offset: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub table: String,
    pub assignments: Vec<(String, Expr)>,
    pub where_: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Delete {
    pub table: String,
    pub where_: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Literal),
    Column { table: Option<String>, name: String },
    Call {
        name: String,
        distinct: bool,
        args: Vec<Expr>,
    },
    Unary(UnaryOp, Box<Expr>),
    Binary(Box<Expr>, BinaryOp, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Number(String),
    String(String),
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

// --- helpers used by the generated grammar's semantic actions ---

pub(crate) fn bin(lhs: Expr, op: BinaryOp, rhs: Expr) -> Expr {
    Expr::Binary(Box::new(lhs), op, Box::new(rhs))
}

/// Strip the surrounding quote characters from a lexed literal.
pub(crate) fn unquote(s: &str, quote: char) -> String {
    s.trim_matches(quote).to_string()
}
