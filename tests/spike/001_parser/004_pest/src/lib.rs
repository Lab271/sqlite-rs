//! sqlite-rs parser spike, variant 004: `pest` (PEG).
//!
//! Grammar lives in `src/grammar.pest`; this module turns pest's `Pairs`
//! (a flat, untyped parse tree) into a minimal AST, just enough to prove the
//! grammar -> AST conversion works for the 5 statement kinds + expressions.

use pest::iterators::Pair;
use pest::Parser as _;

#[derive(pest_derive::Parser)]
#[grammar = "grammar.pest"]
pub struct SqlParser;

pub type ParseError = pest::error::Error<Rule>;

// ===================== AST =====================

#[derive(Debug, Clone, PartialEq)]
// Spike AST: `Select` is much larger than the other variants; boxing it is a
// production concern, not something this comparison needs to model.
#[allow(clippy::large_enum_variant)]
pub enum Stmt {
    CreateTable {
        table: String,
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
        sets: Vec<(String, Expr)>,
        filter: Option<Expr>,
    },
    Delete {
        table: String,
        filter: Option<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub type_name: Option<String>,
    pub not_null: bool,
    pub primary_key: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Select {
    pub distinct: bool,
    pub all: bool,
    pub columns: Vec<ResultColumn>,
    pub from: Option<String>,
    pub filter: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub order_by: Vec<(Expr, Order)>,
    pub limit: Option<Expr>,
    pub offset: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResultColumn {
    Star,
    Expr { expr: Expr, alias: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Order {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Null,
    Number(String),
    String(String),
    Column {
        table: Option<String>,
        name: String,
    },
    Func {
        name: String,
        distinct: bool,
        args: Vec<Expr>,
    },
    /// `op` is one of `NOT`, `+`, `-`.
    Unary { op: String, expr: Box<Expr> },
    /// `op` is one of `OR AND = != <> < <= > >= + - * / % ||`.
    Binary {
        op: String,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
}

// ===================== Entry point =====================

/// Parse exactly one statement (an optional trailing `;` is allowed).
pub fn parse(sql: &str) -> Result<Stmt, ParseError> {
    let statement = SqlParser::parse(Rule::statement, sql)?.next().unwrap();
    let stmt = child(&statement, Rule::stmt).expect("statement always has a stmt child");
    Ok(build_stmt(stmt))
}

// ===================== Pair helpers =====================
//
// pest gives an untyped tree, so navigation is by rule name. Looking children up
// by rule (rather than by position) keeps the keyword token pairs -- which the
// grammar cannot silence, see grammar.pest -- from getting in the way.

fn child<'a>(p: &Pair<'a, Rule>, rule: Rule) -> Option<Pair<'a, Rule>> {
    p.clone().into_inner().find(|c| c.as_rule() == rule)
}

fn children<'a>(p: &Pair<'a, Rule>, rule: Rule) -> Vec<Pair<'a, Rule>> {
    p.clone()
        .into_inner()
        .filter(|c| c.as_rule() == rule)
        .collect()
}

fn has(p: &Pair<Rule>, rule: Rule) -> bool {
    child(p, rule).is_some()
}

fn text(p: &Pair<Rule>) -> String {
    p.as_str().to_string()
}

/// Text of a `table_name` / `column_name` / `identifier`, unquoting `"..."`.
fn name(p: &Pair<Rule>) -> String {
    let s = p.as_str().trim();
    match s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        Some(inner) => inner.to_string(),
        None => s.to_string(),
    }
}

fn only(p: Pair<Rule>) -> Pair<Rule> {
    p.into_inner().next().expect("rule has one inner pair")
}

// ===================== Statements =====================

fn build_stmt(stmt: Pair<Rule>) -> Stmt {
    let p = only(stmt);
    match p.as_rule() {
        Rule::create_table_stmt => Stmt::CreateTable {
            table: name(&child(&p, Rule::table_name).unwrap()),
            if_not_exists: has(&p, Rule::if_not_exists),
            columns: children(&p, Rule::column_def)
                .iter()
                .map(build_column_def)
                .collect(),
        },
        Rule::insert_stmt => Stmt::Insert {
            table: name(&child(&p, Rule::table_name).unwrap()),
            columns: match child(&p, Rule::column_list) {
                Some(list) => children(&list, Rule::column_name).iter().map(name).collect(),
                None => Vec::new(),
            },
            rows: children(&p, Rule::values_row)
                .iter()
                .map(|row| build_expr_list(&child(row, Rule::expr_list).unwrap()))
                .collect(),
        },
        Rule::select_stmt => Stmt::Select(build_select(&p)),
        Rule::update_stmt => Stmt::Update {
            table: name(&child(&p, Rule::table_name).unwrap()),
            sets: children(&p, Rule::assignment)
                .iter()
                .map(|a| {
                    (
                        name(&child(a, Rule::column_name).unwrap()),
                        build_expr(child(a, Rule::expr).unwrap()),
                    )
                })
                .collect(),
            filter: filter_of(&p),
        },
        Rule::delete_stmt => Stmt::Delete {
            table: name(&child(&p, Rule::table_name).unwrap()),
            filter: filter_of(&p),
        },
        other => unreachable!("unexpected statement rule {other:?}"),
    }
}

fn filter_of(p: &Pair<Rule>) -> Option<Expr> {
    child(p, Rule::where_clause).map(|w| build_expr(child(&w, Rule::expr).unwrap()))
}

fn build_column_def(p: &Pair<Rule>) -> ColumnDef {
    let mut def = ColumnDef {
        name: name(&child(p, Rule::column_name).unwrap()),
        // `.trim()`: a rule's span swallows the implicit whitespace consumed by a
        // repetition's last, failed attempt, so `a INTEGER,` spans "INTEGER ".
        type_name: child(p, Rule::type_name).map(|t| t.as_str().trim().to_string()),
        not_null: false,
        primary_key: false,
    };
    for c in children(p, Rule::column_constraint) {
        match only(c).as_rule() {
            Rule::not_null => def.not_null = true,
            Rule::primary_key => def.primary_key = true,
            other => unreachable!("unexpected constraint {other:?}"),
        }
    }
    def
}

fn build_select(p: &Pair<Rule>) -> Select {
    let mut select = Select::default();

    if let Some(mode) = child(p, Rule::select_mode) {
        match only(mode).as_rule() {
            Rule::kw_distinct => select.distinct = true,
            _ => select.all = true,
        }
    }

    for rc in children(p, Rule::result_column) {
        if has(&rc, Rule::star) {
            select.columns.push(ResultColumn::Star);
        } else {
            select.columns.push(ResultColumn::Expr {
                expr: build_expr(child(&rc, Rule::expr).unwrap()),
                alias: child(&rc, Rule::alias)
                    .and_then(|a| child(&a, Rule::identifier).map(|i| name(&i))),
            });
        }
    }

    select.from = child(p, Rule::from_clause).map(|f| name(&child(&f, Rule::table_name).unwrap()));
    select.filter = filter_of(p);

    if let Some(g) = child(p, Rule::group_clause) {
        select.group_by = build_expr_list(&child(&g, Rule::expr_list).unwrap());
        select.having = child(&g, Rule::having_clause)
            .map(|h| build_expr(child(&h, Rule::expr).unwrap()));
    }

    for term in children(p, Rule::order_clause)
        .iter()
        .flat_map(|o| children(o, Rule::ordering_term))
    {
        let dir = if has(&term, Rule::kw_desc) {
            Order::Desc
        } else {
            Order::Asc
        };
        select
            .order_by
            .push((build_expr(child(&term, Rule::expr).unwrap()), dir));
    }

    if let Some(l) = child(p, Rule::limit_clause) {
        let mut exprs = children(&l, Rule::expr).into_iter();
        select.limit = exprs.next().map(build_expr);
        select.offset = exprs.next().map(build_expr);
    }

    select
}

// ===================== Expressions =====================

fn build_expr_list(p: &Pair<Rule>) -> Vec<Expr> {
    children(p, Rule::expr).into_iter().map(build_expr).collect()
}

fn bin(op: &str, lhs: Expr, rhs: Expr) -> Expr {
    Expr::Binary {
        op: op.to_string(),
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

/// Left-fold `a OP b OP c` where OP is a fixed keyword/literal: the operands are
/// simply every child with rule `operand`.
fn fold_fixed(p: Pair<Rule>, operand: Rule, op: &str) -> Expr {
    let mut it = p.into_inner().filter(|c| c.as_rule() == operand);
    let mut lhs = build_expr(it.next().expect("at least one operand"));
    for rhs in it {
        lhs = bin(op, lhs, build_expr(rhs));
    }
    lhs
}

/// Left-fold `a OP b OP c` where OP is its own rule: children alternate
/// operand, op, operand, ...
fn fold_ops(p: Pair<Rule>) -> Expr {
    let mut it = p.into_inner();
    let mut lhs = build_expr(it.next().expect("at least one operand"));
    while let Some(op) = it.next() {
        let op = text(&op);
        let rhs = build_expr(it.next().expect("operator needs a right operand"));
        lhs = bin(&op, lhs, rhs);
    }
    lhs
}

fn build_expr(p: Pair<Rule>) -> Expr {
    match p.as_rule() {
        Rule::expr | Rule::primary_expr => build_expr(only(p)),
        Rule::or_expr => fold_fixed(p, Rule::and_expr, "OR"),
        Rule::and_expr => fold_fixed(p, Rule::not_expr, "AND"),
        Rule::concat_expr => fold_fixed(p, Rule::unary_expr, "||"),
        Rule::comparison_expr | Rule::additive_expr | Rule::multiplicative_expr => fold_ops(p),
        Rule::not_expr => {
            let mut it = p.into_inner();
            let first = it.next().unwrap();
            if first.as_rule() == Rule::kw_not {
                Expr::Unary {
                    op: "NOT".to_string(),
                    expr: Box::new(build_expr(it.next().unwrap())),
                }
            } else {
                build_expr(first)
            }
        }
        Rule::unary_expr => {
            let mut it = p.into_inner();
            let first = it.next().unwrap();
            if first.as_rule() == Rule::unary_op {
                Expr::Unary {
                    op: text(&first),
                    expr: Box::new(build_expr(it.next().unwrap())),
                }
            } else {
                build_expr(first)
            }
        }
        Rule::paren_expr => build_expr(child(&p, Rule::expr).unwrap()),
        Rule::literal => {
            let inner = only(p);
            match inner.as_rule() {
                Rule::number => Expr::Number(text(&inner)),
                Rule::string => {
                    let raw = inner.as_str();
                    Expr::String(raw[1..raw.len() - 1].replace("''", "'"))
                }
                Rule::kw_null => Expr::Null,
                other => unreachable!("unexpected literal {other:?}"),
            }
        }
        Rule::column_ref => {
            let ids = children(&p, Rule::identifier);
            if ids.len() == 2 {
                Expr::Column {
                    table: Some(name(&ids[0])),
                    name: name(&ids[1]),
                }
            } else {
                Expr::Column {
                    table: None,
                    name: name(&ids[0]),
                }
            }
        }
        Rule::function_call => Expr::Func {
            name: name(&child(&p, Rule::identifier).unwrap()),
            distinct: has(&p, Rule::kw_distinct),
            args: child(&p, Rule::expr_list)
                .map(|l| build_expr_list(&l))
                .unwrap_or_default(),
        },
        other => unreachable!("unexpected expression rule {other:?}"),
    }
}
