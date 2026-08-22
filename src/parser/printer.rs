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
        if !self.group_by.is_empty() {
            write!(f, " GROUP BY ")?;
            for (i, expr) in self.group_by.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{expr}")?;
            }
        }
        if let Some(having) = &self.having {
            write!(f, " HAVING {having}")?;
        }
        for arm in &self.compound {
            write!(f, " {arm}")?;
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

impl fmt::Display for CompoundSelect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.op {
            CompoundOp::UnionAll => write!(f, "UNION ALL SELECT")?,
        }
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
        if !self.group_by.is_empty() {
            write!(f, " GROUP BY ")?;
            for (i, expr) in self.group_by.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{expr}")?;
            }
        }
        if let Some(having) = &self.having {
            write!(f, " HAVING {having}")?;
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
        match &self.kind {
            TableRefKind::Name(name) => write!(f, "{name}")?,
            TableRefKind::Subquery(select) => write!(f, "({select})")?,
        }
        if let Some(alias) = &self.alias {
            write!(f, " AS {alias}")?;
        }
        Ok(())
    }
}

impl fmt::Display for FromClause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.first)?;
        for join in &self.joins {
            write!(f, " {join}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Join {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op = match self.op {
            JoinOp::Inner => "JOIN",
            JoinOp::Left => "LEFT JOIN",
            JoinOp::Cross => "CROSS JOIN",
            JoinOp::Right => "RIGHT JOIN",
            JoinOp::Full => "FULL JOIN",
        };
        if self.natural {
            write!(f, "NATURAL ")?;
        }
        write!(f, "{op} {}", self.table)?;
        match &self.constraint {
            Some(JoinConstraint::On(expr)) => write!(f, " ON {expr}")?,
            Some(JoinConstraint::Using(cols)) => write!(f, " USING ({})", cols.join(", "))?,
            None => {}
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
            ExprKind::Subquery(select) => write!(f, "({select})"),
            ExprKind::Exists { subquery, negated } => {
                if *negated {
                    write!(f, "NOT EXISTS ({subquery})")
                } else {
                    write!(f, "EXISTS ({subquery})")
                }
            }
            ExprKind::InSubquery {
                expr,
                subquery,
                negated,
            } => {
                if *negated {
                    write!(f, "{expr} NOT IN ({subquery})")
                } else {
                    write!(f, "{expr} IN ({subquery})")
                }
            }
            ExprKind::InSubqueryMulti {
                exprs,
                subquery,
                negated,
            } => {
                write!(f, "(")?;
                for (i, e) in exprs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{e}")?;
                }
                write!(f, ")")?;
                if *negated {
                    write!(f, " NOT IN ({subquery})")
                } else {
                    write!(f, " IN ({subquery})")
                }
            }
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

impl fmt::Display for Delete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DELETE FROM {}", self.table)?;
        if let Some(w) = &self.where_clause {
            write!(f, " WHERE {w}")?;
        }
        Ok(())
    }
}

impl fmt::Display for CreateTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CREATE TABLE ")?;
        if self.if_not_exists {
            write!(f, "IF NOT EXISTS ")?;
        }
        write!(f, "{} (", self.name)?;
        for (i, col) in self.columns.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{col}")?;
        }
        for constraint in &self.constraints {
            write!(f, ", {constraint}")?;
        }
        write!(f, ")")?;
        if self.without_rowid {
            write!(f, " WITHOUT ROWID")?;
        } else if self.strict {
            write!(f, " STRICT")?;
        }
        Ok(())
    }
}

impl fmt::Display for ColumnDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if let Some(type_name) = &self.type_name {
            write!(f, " {type_name}")?;
        }
        for constraint in &self.constraints {
            write!(f, " {constraint}")?;
        }
        Ok(())
    }
}

impl fmt::Display for ColumnConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ColumnConstraint::NotNull => write!(f, "NOT NULL"),
            ColumnConstraint::PrimaryKey {
                desc,
                autoincrement,
            } => {
                write!(f, "PRIMARY KEY")?;
                match desc {
                    Some(false) => write!(f, " ASC")?,
                    Some(true) => write!(f, " DESC")?,
                    None => {}
                }
                if *autoincrement {
                    write!(f, " AUTOINCREMENT")?;
                }
                Ok(())
            }
            ColumnConstraint::Unique => write!(f, "UNIQUE"),
            ColumnConstraint::Default(value) => write!(f, "DEFAULT {value}"),
            ColumnConstraint::Check(expr) => write!(f, "CHECK ({expr})"),
            ColumnConstraint::Collate(name) => write!(f, "COLLATE {name}"),
        }
    }
}

impl fmt::Display for DefaultValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DefaultValue::Literal(expr) => write!(f, "{expr}"),
            DefaultValue::Paren(expr) => write!(f, "({expr})"),
        }
    }
}

impl fmt::Display for TableConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TableConstraint::PrimaryKey(cols) => {
                write!(f, "PRIMARY KEY (")?;
                write_indexed_columns(f, cols)?;
                write!(f, ")")
            }
            TableConstraint::Unique(cols) => {
                write!(f, "UNIQUE (")?;
                write_indexed_columns(f, cols)?;
                write!(f, ")")
            }
            TableConstraint::Check(expr) => write!(f, "CHECK ({expr})"),
        }
    }
}

fn write_indexed_columns(f: &mut fmt::Formatter<'_>, cols: &[IndexedColumn]) -> fmt::Result {
    for (i, col) in cols.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{col}")?;
    }
    Ok(())
}

impl fmt::Display for IndexedColumn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.expr)?;
        match self.desc {
            Some(false) => write!(f, " ASC")?,
            Some(true) => write!(f, " DESC")?,
            None => {}
        }
        Ok(())
    }
}

impl fmt::Display for CreateIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CREATE ")?;
        if self.unique {
            write!(f, "UNIQUE ")?;
        }
        write!(f, "INDEX ")?;
        if self.if_not_exists {
            write!(f, "IF NOT EXISTS ")?;
        }
        write!(f, "{} ON {} (", self.name, self.table)?;
        write_indexed_columns(f, &self.columns)?;
        write!(f, ")")?;
        if let Some(where_clause) = &self.where_clause {
            write!(f, " WHERE {where_clause}")?;
        }
        Ok(())
    }
}

impl fmt::Display for DropTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DROP TABLE ")?;
        if self.if_exists {
            write!(f, "IF EXISTS ")?;
        }
        write!(f, "{}", self.name)
    }
}

impl fmt::Display for DropIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DROP INDEX ")?;
        if self.if_exists {
            write!(f, "IF EXISTS ")?;
        }
        write!(f, "{}", self.name)
    }
}

impl fmt::Display for TransactionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TransactionMode::Deferred => "DEFERRED",
            TransactionMode::Immediate => "IMMEDIATE",
            TransactionMode::Exclusive => "EXCLUSIVE",
        };
        write!(f, "{s}")
    }
}

impl fmt::Display for Begin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BEGIN")?;
        if let Some(mode) = self.mode {
            write!(f, " {mode}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Commit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "COMMIT")
    }
}

impl fmt::Display for Rollback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ROLLBACK")
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
