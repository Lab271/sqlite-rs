//! Pretty-printer for the V2 AST, used to verify the roundtrip
//! requirement (spec 002-parser Requirement 3: "parse -> print -> parse
//! gives identical AST"). Always emits explicit parentheses around
//! `ExprKind::Paren` nodes and normalizes whitespace/casing, so printer
//! output is not expected to match the original source text verbatim —
//! only to reparse to the same AST.

use super::ast::*;
use std::fmt;

impl fmt::Display for Select {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SELECT")?;
        match self.distinct {
            Some(Distinctness::Distinct) => write!(f, " DISTINCT")?,
            Some(Distinctness::All) => write!(f, " ALL")?,
            None => {}
        }
        for (i, col) in self.columns.iter().enumerate() {
            if i == 0 {
                write!(f, " ")?;
            } else {
                write!(f, ", ")?;
            }
            write!(f, "{col}")?;
        }
        if let Some(from) = &self.from {
            write!(f, " FROM {from}")?;
        }
        if let Some(w) = &self.where_clause {
            write!(f, " WHERE {w}")?;
        }
        if !self.order_by.is_empty() {
            write!(f, " ORDER BY ")?;
            for (i, term) in self.order_by.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{term}")?;
            }
        }
        if let Some(limit) = &self.limit {
            write!(f, " LIMIT {}", limit.limit)?;
            if let Some(offset) = &limit.offset {
                write!(f, " OFFSET {offset}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for ResultColumn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResultColumn::Star => write!(f, "*"),
            ResultColumn::TableStar { table } => write!(f, "{table}.*"),
            ResultColumn::Expr { expr, alias } => {
                write!(f, "{expr}")?;
                if let Some(alias) = alias {
                    write!(f, " AS {alias}")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for TableRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if let Some(alias) = &self.alias {
            write!(f, " AS {alias}")?;
        }
        Ok(())
    }
}

impl fmt::Display for OrderingTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.expr)?;
        match self.desc {
            Some(false) => write!(f, " ASC")?,
            Some(true) => write!(f, " DESC")?,
            None => {}
        }
        match self.nulls_last {
            Some(false) => write!(f, " NULLS FIRST")?,
            Some(true) => write!(f, " NULLS LAST")?,
            None => {}
        }
        Ok(())
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ExprKind::Literal(lit) => write!(f, "{lit}"),
            ExprKind::Param(p) => write!(f, "{p}"),
            ExprKind::Column {
                catalog,
                table,
                name,
            } => {
                if let Some(catalog) = catalog {
                    write!(f, "{catalog}.")?;
                }
                if let Some(table) = table {
                    write!(f, "{table}.")?;
                }
                write!(f, "{name}")
            }
            ExprKind::FunctionCall {
                name,
                distinct,
                args,
            } => {
                write!(f, "{name}(")?;
                if *distinct {
                    write!(f, "DISTINCT ")?;
                }
                match args {
                    FunctionArgs::Star => write!(f, "*")?,
                    FunctionArgs::List(list) => {
                        for (i, a) in list.iter().enumerate() {
                            if i > 0 {
                                write!(f, ", ")?;
                            }
                            write!(f, "{a}")?;
                        }
                    }
                }
                write!(f, ")")
            }
            ExprKind::Unary { op, expr } => {
                let op = match op {
                    UnaryOp::Not => "NOT ",
                    UnaryOp::Plus => "+",
                    UnaryOp::Minus => "-",
                    UnaryOp::BitNot => "~",
                };
                write!(f, "{op}{expr}")
            }
            ExprKind::Binary { op, lhs, rhs } => {
                write!(f, "{lhs} {} {rhs}", binop_str(*op))
            }
            ExprKind::Is { lhs, rhs, negated } => {
                write!(f, "{lhs} IS {}{rhs}", if *negated { "NOT " } else { "" })
            }
            ExprKind::IsNull { expr, negated } => {
                write!(f, "{expr} {}", if *negated { "NOTNULL" } else { "ISNULL" })
            }
            ExprKind::Between {
                expr,
                lo,
                hi,
                negated,
            } => write!(
                f,
                "{expr} {}BETWEEN {lo} AND {hi}",
                if *negated { "NOT " } else { "" }
            ),
            ExprKind::In {
                expr,
                list,
                negated,
            } => {
                write!(f, "{expr} {}IN (", if *negated { "NOT " } else { "" })?;
                for (i, e) in list.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{e}")?;
                }
                write!(f, ")")
            }
            ExprKind::Like {
                expr,
                pattern,
                glob,
                negated,
                escape,
            } => {
                let op = if *glob { "GLOB" } else { "LIKE" };
                write!(
                    f,
                    "{expr} {}{op} {pattern}",
                    if *negated { "NOT " } else { "" }
                )?;
                if let Some(escape) = escape {
                    write!(f, " ESCAPE {escape}")?;
                }
                Ok(())
            }
            ExprKind::Case {
                operand,
                whens,
                else_,
            } => {
                write!(f, "CASE")?;
                if let Some(operand) = operand {
                    write!(f, " {operand}")?;
                }
                for (cond, res) in whens {
                    write!(f, " WHEN {cond} THEN {res}")?;
                }
                if let Some(else_) = else_ {
                    write!(f, " ELSE {else_}")?;
                }
                write!(f, " END")
            }
            ExprKind::Cast { expr, type_name } => write!(f, "CAST({expr} AS {type_name})"),
            ExprKind::Collate { expr, collation } => write!(f, "{expr} COLLATE {collation}"),
            ExprKind::Paren(inner) => write!(f, "({inner})"),
        }
    }
}

fn binop_str(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Or => "OR",
        BinaryOp::And => "AND",
        BinaryOp::Eq => "=",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::Shl => "<<",
        BinaryOp::Shr => ">>",
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::Concat => "||",
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Literal::Integer(v) => write!(f, "{v}"),
            Literal::Float(v) => write!(f, "{v}"),
            Literal::Str(s) => write!(f, "'{}'", s.replace('\'', "''")),
            Literal::Blob(bytes) => {
                write!(f, "X'")?;
                for b in bytes {
                    write!(f, "{b:02X}")?;
                }
                write!(f, "'")
            }
            Literal::Null => write!(f, "NULL"),
            Literal::True => write!(f, "TRUE"),
            Literal::False => write!(f, "FALSE"),
        }
    }
}

impl fmt::Display for Insert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "INSERT")?;
        if let Some(action) = self.or_action {
            let action = match action {
                ConflictAction::Replace => "REPLACE",
                ConflictAction::Ignore => "IGNORE",
                ConflictAction::Abort => "ABORT",
                ConflictAction::Rollback => "ROLLBACK",
                ConflictAction::Fail => "FAIL",
            };
            write!(f, " OR {action}")?;
        }
        write!(f, " INTO {}", self.table)?;
        if let Some(columns) = &self.columns {
            write!(f, " (")?;
            for (i, col) in columns.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{col}")?;
            }
            write!(f, ")")?;
        }
        match &self.source {
            InsertSource::DefaultValues => write!(f, " DEFAULT VALUES"),
            InsertSource::Values(rows) => {
                write!(f, " VALUES ")?;
                for (i, row) in rows.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "(")?;
                    for (j, expr) in row.iter().enumerate() {
                        if j > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{expr}")?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            InsertSource::Select(select) => write!(f, " {select}"),
        }
    }
}

impl fmt::Display for ParamKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParamKind::Anonymous => write!(f, "?"),
            ParamKind::Numbered(n) => write!(f, "?{n}"),
            ParamKind::Colon(s) => write!(f, ":{s}"),
            ParamKind::At(s) => write!(f, "@{s}"),
            ParamKind::Dollar(s) => write!(f, "${s}"),
        }
    }
}
