//! Recursive-descent parser for the SELECT-core V2 slice (spec
//! 002-parser Requirements 2-4; grammar `.openspec/grammar/sqlite.ebnf`
//! V2 block). Hand-written rather than pomelo/lemon-generated: spike
//! 006 (#57) picked pomelo as the long-term generator, but this ticket
//! ships the V2 slice directly against the tokenizer to keep the
//! surface small; a generator swap is future work and does not change
//! the AST or diagnostics contract.
//!
//! Operator precedence (lowest to highest) mirrors `parse.y`'s
//! `%left`/`%right` declarations exactly — see the grammar file's
//! "Expressions (V2 slice)" section for the full table. Descending-
//! precedence call order: `expr` (guarded) -> `bool_expr` (OR/AND,
//! precedence-climbing) -> `not_expr` (guarded) -> `equality_expr` ->
//! `binary_expr` (relational/bitwise/additive/multiplicative/concat,
//! precedence-climbing) -> `arrow_expr` -> `collate_expr` -> `unary_expr`
//! (guarded) -> `primary_expr`.
//!
//! `bool_expr` and `binary_expr` each collapse several historically
//! separate pass-through levels (OR+AND; relational/bitwise/additive/
//! multiplicative/concat) into one precedence-climbing function apiece —
//! one stack frame per nesting level instead of one per former level. That
//! collapse, plus `#118`'s narrower per-level cost, is what lets
//! `MAX_EXPR_DEPTH` actually be reached (rather than stack-overflowing
//! first) within a debug build's default thread stack.

use super::ast::*;
use super::error::{PResult, ParseFail};
use super::tokenizer::{Keyword, Param, Span, Token, TokenKind};

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

/// Recursion-depth cap for `expr`/`not_expr`/`unary_expr`, so pathological
/// input (many nested `(`, or repeated `NOT`/unary operators) returns a
/// clean `ParseFail::Invalid` instead of overflowing the stack.
const MAX_EXPR_DEPTH: usize = 200;

fn join_span(a: Span, b: Span) -> Span {
    Span {
        line: a.line,
        column: a.column,
        offset: a.offset,
        len: b.offset.saturating_add(b.len).saturating_sub(a.offset),
    }
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser {
            tokens,
            pos: 0,
            depth: 0,
        }
    }

    /// Guards a recursive-descent entry point: increments the depth
    /// counter, fails with `Invalid` past `MAX_EXPR_DEPTH` instead of
    /// recursing further, and always decrements again afterward
    /// (including on error) so sibling subtrees aren't penalized.
    fn with_depth_guard<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        self.depth = self.depth.saturating_add(1);
        if self.depth > MAX_EXPR_DEPTH {
            self.depth = self.depth.saturating_sub(1);
            return self.invalid("expression nesting too deep");
        }
        let result = f(self);
        self.depth = self.depth.saturating_sub(1);
        result
    }

    // ---- token stream helpers ----------------------------------------

    fn peek(&self) -> &Token {
        self.peek_at(0)
    }

    fn peek_at(&self, offset: usize) -> &Token {
        let idx = self.pos.saturating_add(offset);
        self.tokens.get(idx).unwrap_or_else(|| {
            // The tokenizer always terminates its stream with `Eof`, so
            // any in-range index resolves; out-of-range only happens by
            // peeking past `Eof`, which we handle by returning the last
            // (`Eof`) token itself.
            self.tokens.last().unwrap_or(&Token {
                kind: TokenKind::Eof,
                span: Span {
                    line: 1,
                    column: 1,
                    offset: 0,
                    len: 0,
                },
            })
        })
    }

    fn advance(&mut self) -> Token {
        let tok = self.peek().clone();
        if !matches!(tok.kind, TokenKind::Eof) {
            self.pos = self.pos.saturating_add(1);
        }
        tok
    }

    fn at_kw(&self, kw: Keyword) -> bool {
        matches!(&self.peek().kind, TokenKind::Keyword(k) if *k == kw)
    }

    fn eat_kw(&mut self, kw: Keyword) -> bool {
        if self.at_kw(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_kw(&mut self, kw: Keyword) -> PResult<Span> {
        if self.at_kw(kw) {
            Ok(self.advance().span)
        } else {
            let tok = self.peek().clone();
            Err(ParseFail::Invalid {
                message: format!("expected {kw:?}, found {:?}", tok.kind),
                span: tok.span,
            })
        }
    }

    fn eat_punct(&mut self, kind: &TokenKind) -> bool {
        if &self.peek().kind == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, kind: TokenKind, what: &str) -> PResult<Span> {
        if self.peek().kind == kind {
            Ok(self.advance().span)
        } else {
            let tok = self.peek().clone();
            Err(ParseFail::Invalid {
                message: format!("expected {what}, found {:?}", tok.kind),
                span: tok.span,
            })
        }
    }

    fn invalid<T>(&self, message: impl Into<String>) -> PResult<T> {
        Err(ParseFail::Invalid {
            message: message.into(),
            span: self.peek().span,
        })
    }

    fn unsupported<T>(&self, message: impl Into<String>) -> PResult<T> {
        Err(ParseFail::Unsupported {
            message: message.into(),
            span: self.peek().span,
        })
    }

    /// After a full statement is parsed, only a trailing `;` (optionally
    /// repeated) and EOF are allowed.
    pub(super) fn expect_end(&mut self) -> PResult<()> {
        while self.eat_punct(&TokenKind::Semicolon) {}
        match &self.peek().kind {
            TokenKind::Eof => Ok(()),
            TokenKind::Keyword(Keyword::UNION)
            | TokenKind::Keyword(Keyword::INTERSECT)
            | TokenKind::Keyword(Keyword::EXCEPT) => {
                self.unsupported("compound SELECT (UNION/INTERSECT/EXCEPT) not yet supported")
            }
            other => {
                let tok = other.clone();
                Err(ParseFail::Invalid {
                    message: format!("unexpected trailing token {tok:?}"),
                    span: self.peek().span,
                })
            }
        }
    }

    fn identifier(&mut self) -> PResult<(String, Span)> {
        match self.peek().kind.clone() {
            TokenKind::Identifier(name) => {
                let span = self.advance().span;
                Ok((name, span))
            }
            _ => {
                let tok = self.peek().clone();
                Err(ParseFail::Invalid {
                    message: format!("expected identifier, found {:?}", tok.kind),
                    span: tok.span,
                })
            }
        }
    }

    // ---- statement -----------------------------------------------------

    pub(super) fn parse_insert_stmt(&mut self) -> PResult<Insert> {
        let start = self.expect_kw(Keyword::INSERT)?;
        let or_action = if self.eat_kw(Keyword::OR) {
            Some(self.conflict_action()?)
        } else {
            None
        };
        self.expect_kw(Keyword::INTO)?;
        let (table, _) = self.identifier()?;

        let columns = if self.eat_punct(&TokenKind::LParen) {
            let mut cols = vec![self.identifier()?.0];
            while self.eat_punct(&TokenKind::Comma) {
                cols.push(self.identifier()?.0);
            }
            self.expect_punct(TokenKind::RParen, "')'")?;
            Some(cols)
        } else {
            None
        };

        let (source, end) = if self.eat_kw(Keyword::DEFAULT) {
            let end = self.expect_kw(Keyword::VALUES)?;
            (InsertSource::DefaultValues, end)
        } else if self.eat_kw(Keyword::VALUES) {
            let first_row = self.value_row()?;
            // `expr_list` always yields at least one element, so `last()` is safe.
            #[allow(clippy::expect_used)]
            let mut end = first_row.last().expect("value row is non-empty").span;
            let mut rows = vec![first_row];
            while self.eat_punct(&TokenKind::Comma) {
                let row = self.value_row()?;
                if let Some(last) = row.last() {
                    end = last.span;
                }
                rows.push(row);
            }
            (InsertSource::Values(rows), end)
        } else if self.at_kw(Keyword::SELECT) || self.at_kw(Keyword::WITH) {
            let select = self.parse_select_stmt()?;
            let end = select.span;
            (InsertSource::Select(Box::new(select)), end)
        } else {
            return self.invalid("expected VALUES, DEFAULT VALUES, or SELECT after INSERT INTO");
        };

        Ok(Insert {
            or_action,
            table,
            columns,
            source,
            span: join_span(start, end),
        })
    }

    pub(super) fn parse_delete_stmt(&mut self) -> PResult<Delete> {
        let start = self.expect_kw(Keyword::DELETE)?;
        self.expect_kw(Keyword::FROM)?;
        let (table, table_span) = self.identifier()?;

        let mut end = table_span;
        let where_clause = if self.eat_kw(Keyword::WHERE) {
            let expr = self.expr()?;
            end = expr.span;
            Some(expr)
        } else {
            None
        };

        Ok(Delete {
            table,
            where_clause,
            span: join_span(start, end),
        })
    }

    fn conflict_action(&mut self) -> PResult<ConflictAction> {
        if self.eat_kw(Keyword::REPLACE) {
            Ok(ConflictAction::Replace)
        } else if self.eat_kw(Keyword::IGNORE) {
            Ok(ConflictAction::Ignore)
        } else if self.eat_kw(Keyword::ABORT) {
            Ok(ConflictAction::Abort)
        } else if self.eat_kw(Keyword::ROLLBACK) {
            Ok(ConflictAction::Rollback)
        } else if self.eat_kw(Keyword::FAIL) {
            Ok(ConflictAction::Fail)
        } else {
            self.invalid("expected REPLACE, IGNORE, ABORT, ROLLBACK, or FAIL after OR")
        }
    }

    fn value_row(&mut self) -> PResult<Vec<Expr>> {
        self.expect_punct(TokenKind::LParen, "'('")?;
        let list = self.expr_list()?;
        self.expect_punct(TokenKind::RParen, "')'")?;
        Ok(list)
    }

    /// `update-stmt` (grammar V3 block): `UPDATE [OR conflict-action]
    /// table-name SET assignment { "," assignment } [ WHERE expr ]`, where
    /// `assignment` is either `column-name "=" expr` or the tuple form
    /// `"(" column-name { "," column-name } ")" "=" "(" expr-list ")"`.
    pub(super) fn parse_update_stmt(&mut self) -> PResult<Update> {
        let start = self.expect_kw(Keyword::UPDATE)?;

        let or_action = if self.eat_kw(Keyword::OR) {
            Some(self.conflict_action()?)
        } else {
            None
        };

        let (table, _) = self.identifier()?;

        self.expect_kw(Keyword::SET)?;

        let mut assignments = self.assignment()?;
        while self.eat_punct(&TokenKind::Comma) {
            assignments.extend(self.assignment()?);
        }

        let where_clause = if self.eat_kw(Keyword::WHERE) {
            Some(self.expr()?)
        } else {
            None
        };

        let end = self
            .tokens
            .get(self.pos.saturating_sub(1))
            .map_or(start, |t| t.span);
        Ok(Update {
            or_action,
            table,
            assignments,
            where_clause,
            span: join_span(start, end),
        })
    }

    /// One assignment "slot": `column-name "=" expr` (yields one
    /// [`Assignment`]), or the tuple form
    /// `"(" column-name { "," column-name } ")" "=" "(" expr-list ")"`,
    /// which requires a matching-arity parenthesized RHS expr-list (a
    /// scalar-subquery RHS is not yet supported) and expands into one
    /// [`Assignment`] per column, each paired with its RHS expr.
    fn assignment(&mut self) -> PResult<Vec<Assignment>> {
        if matches!(self.peek().kind, TokenKind::LParen) {
            self.advance();
            let mut columns = vec![self.identifier()?.0];
            while self.eat_punct(&TokenKind::Comma) {
                columns.push(self.identifier()?.0);
            }
            self.expect_punct(TokenKind::RParen, "')' to close column list")?;
            self.expect_punct(TokenKind::Eq, "'=' in tuple assignment")?;
            if !matches!(self.peek().kind, TokenKind::LParen) {
                return self.unsupported("tuple assignment RHS must be a parenthesized expr-list");
            }
            self.advance();
            if self.at_kw(Keyword::SELECT) {
                return self.unsupported("tuple assignment RHS subquery not yet supported");
            }
            let values = self.expr_list()?;
            self.expect_punct(TokenKind::RParen, "')' to close tuple assignment")?;
            if values.len() != columns.len() {
                return self.invalid("tuple assignment column/value count mismatch");
            }
            return Ok(columns
                .into_iter()
                .zip(values)
                .map(|(name, value)| Assignment {
                    columns: vec![name],
                    value,
                })
                .collect());
        }

        let (name, _) = self.identifier()?;
        self.expect_punct(TokenKind::Eq, "'=' in assignment")?;
        let value = self.expr()?;
        Ok(vec![Assignment {
            columns: vec![name],
            value,
        }])
    }

    // ---- DDL: CREATE/DROP TABLE, CREATE/DROP INDEX -----------------------

    fn opt_if_not_exists(&mut self) -> PResult<bool> {
        if self.eat_kw(Keyword::IF) {
            self.expect_kw(Keyword::NOT)?;
            self.expect_kw(Keyword::EXISTS)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn opt_if_exists(&mut self) -> PResult<bool> {
        if self.eat_kw(Keyword::IF) {
            self.expect_kw(Keyword::EXISTS)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// SQLite treats `ROWID`/`STRICT` as contextual keywords (unreserved
    /// words, not `Keyword` tokens) — matched case-insensitively against a
    /// bare identifier.
    fn eat_contextual_kw(&mut self, word: &str) -> bool {
        if matches!(&self.peek().kind, TokenKind::Identifier(id) if id.eq_ignore_ascii_case(word)) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Bails with `Unsupported` (schema-qualified names not yet supported)
    /// if a `.` follows, mirroring `table_ref`'s existing behavior.
    fn check_no_schema_qualifier(&mut self) -> PResult<()> {
        if matches!(self.peek().kind, TokenKind::Dot) {
            return self.unsupported("schema-qualified names not yet supported");
        }
        Ok(())
    }

    /// Bails with `Unsupported` if an `ON CONFLICT` resolution clause
    /// follows — real SQLite allows one after NOT NULL/PRIMARY KEY/UNIQUE,
    /// but representing it isn't in this ticket's scope.
    fn check_no_conflict_clause(&mut self) -> PResult<()> {
        if self.at_kw(Keyword::ON)
            && matches!(self.peek_at(1).kind, TokenKind::Keyword(Keyword::CONFLICT))
        {
            return self.unsupported("ON CONFLICT resolution clause not yet supported");
        }
        Ok(())
    }

    pub(super) fn parse_create_table_stmt(&mut self) -> PResult<CreateTable> {
        let start = self.expect_kw(Keyword::CREATE)?;
        if self.at_kw(Keyword::TEMP) || self.at_kw(Keyword::TEMPORARY) {
            return self.unsupported("CREATE TEMP/TEMPORARY TABLE not yet supported");
        }
        if self.at_kw(Keyword::VIRTUAL) {
            return self.unsupported("CREATE VIRTUAL TABLE not yet supported");
        }
        self.expect_kw(Keyword::TABLE)?;
        let if_not_exists = self.opt_if_not_exists()?;
        let (name, _) = self.identifier()?;
        self.check_no_schema_qualifier()?;
        if self.at_kw(Keyword::AS) {
            return self.unsupported("CREATE TABLE ... AS select-stmt not yet supported");
        }
        self.expect_punct(TokenKind::LParen, "'(' after table name")?;

        let mut columns = vec![self.column_def()?];
        let mut constraints = Vec::new();
        while self.eat_punct(&TokenKind::Comma) {
            if self.at_table_constraint_start() {
                constraints.push(self.table_constraint()?);
                while self.eat_punct(&TokenKind::Comma) {
                    constraints.push(self.table_constraint()?);
                }
                break;
            }
            columns.push(self.column_def()?);
        }
        let mut end = self.expect_punct(TokenKind::RParen, "')' to close column list")?;

        let mut without_rowid = false;
        let mut strict = false;
        if self.eat_kw(Keyword::WITHOUT) {
            if !self.eat_contextual_kw("ROWID") {
                return self.invalid("expected ROWID after WITHOUT");
            }
            end = self
                .tokens
                .get(self.pos.saturating_sub(1))
                .map_or(end, |t| t.span);
            without_rowid = true;
        } else if matches!(&self.peek().kind, TokenKind::Identifier(id) if id.eq_ignore_ascii_case("STRICT"))
        {
            end = self.advance().span;
            strict = true;
        }

        Ok(CreateTable {
            if_not_exists,
            name,
            columns,
            constraints,
            without_rowid,
            strict,
            span: join_span(start, end),
        })
    }

    fn at_table_constraint_start(&self) -> bool {
        self.at_kw(Keyword::CONSTRAINT)
            || self.at_kw(Keyword::PRIMARY)
            || self.at_kw(Keyword::UNIQUE)
            || self.at_kw(Keyword::CHECK)
            || self.at_kw(Keyword::FOREIGN)
    }

    fn column_def(&mut self) -> PResult<ColumnDef> {
        let (name, _) = self.identifier()?;
        let type_name = if matches!(self.peek().kind, TokenKind::Identifier(_)) {
            Some(self.type_name()?)
        } else {
            None
        };
        let mut constraints = Vec::new();
        while let Some(c) = self.opt_column_constraint()? {
            constraints.push(c);
        }
        Ok(ColumnDef {
            name,
            type_name,
            constraints,
        })
    }

    fn opt_column_constraint(&mut self) -> PResult<Option<ColumnConstraint>> {
        let named = self.eat_kw(Keyword::CONSTRAINT);
        if named {
            self.identifier()?;
        }
        if self.eat_kw(Keyword::NOT) {
            self.expect_punct(TokenKind::Null, "NULL")?;
            self.check_no_conflict_clause()?;
            return Ok(Some(ColumnConstraint::NotNull));
        }
        if matches!(self.peek().kind, TokenKind::Null) {
            return self.unsupported("bare NULL column constraint not yet supported");
        }
        if self.eat_kw(Keyword::PRIMARY) {
            self.expect_kw(Keyword::KEY)?;
            let desc = if self.eat_kw(Keyword::ASC) {
                Some(false)
            } else if self.eat_kw(Keyword::DESC) {
                Some(true)
            } else {
                None
            };
            self.check_no_conflict_clause()?;
            let autoincrement = self.eat_kw(Keyword::AUTOINCREMENT);
            return Ok(Some(ColumnConstraint::PrimaryKey {
                desc,
                autoincrement,
            }));
        }
        if self.eat_kw(Keyword::UNIQUE) {
            self.check_no_conflict_clause()?;
            return Ok(Some(ColumnConstraint::Unique));
        }
        if self.eat_kw(Keyword::CHECK) {
            self.expect_punct(TokenKind::LParen, "'(' after CHECK")?;
            let expr = self.expr()?;
            self.expect_punct(TokenKind::RParen, "')' to close CHECK")?;
            return Ok(Some(ColumnConstraint::Check(expr)));
        }
        if self.eat_kw(Keyword::DEFAULT) {
            return Ok(Some(ColumnConstraint::Default(self.default_value()?)));
        }
        if self.eat_kw(Keyword::COLLATE) {
            let (name, _) = self.identifier()?;
            return Ok(Some(ColumnConstraint::Collate(name)));
        }
        if self.at_kw(Keyword::REFERENCES) {
            return self
                .unsupported("REFERENCES (foreign key) column constraint not yet supported");
        }
        if self.at_kw(Keyword::GENERATED)
            || (self.at_kw(Keyword::AS) && matches!(self.peek_at(1).kind, TokenKind::LParen))
        {
            return self.unsupported("GENERATED ALWAYS AS not yet supported");
        }
        if named {
            return self.invalid("expected column constraint after CONSTRAINT name");
        }
        Ok(None)
    }

    fn default_value(&mut self) -> PResult<DefaultValue> {
        if self.eat_punct(&TokenKind::LParen) {
            let expr = self.expr()?;
            self.expect_punct(TokenKind::RParen, "')' to close DEFAULT expression")?;
            return Ok(DefaultValue::Paren(expr));
        }
        if matches!(self.peek().kind, TokenKind::Plus | TokenKind::Minus) {
            let op = if matches!(self.peek().kind, TokenKind::Minus) {
                UnaryOp::Minus
            } else {
                UnaryOp::Plus
            };
            let start = self.advance().span;
            let inner = self.literal_value()?;
            let span = join_span(start, inner.span);
            return Ok(DefaultValue::Literal(Expr {
                kind: ExprKind::Unary {
                    op,
                    expr: Box::new(inner),
                },
                span,
            }));
        }
        Ok(DefaultValue::Literal(self.literal_value()?))
    }

    /// `literal-value` only (no columns, params, or general expressions) —
    /// the bare (non-parenthesized) form `DEFAULT` accepts.
    fn literal_value(&mut self) -> PResult<Expr> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Integer(v) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::Integer(v)),
                    span: tok.span,
                })
            }
            TokenKind::Float(v) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::Float(v)),
                    span: tok.span,
                })
            }
            TokenKind::String(s) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::Str(s)),
                    span: tok.span,
                })
            }
            TokenKind::Blob(b) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::Blob(b)),
                    span: tok.span,
                })
            }
            TokenKind::Null => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::Null),
                    span: tok.span,
                })
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::True),
                    span: tok.span,
                })
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::False),
                    span: tok.span,
                })
            }
            TokenKind::Keyword(Keyword::CURRENT_TIME)
            | TokenKind::Keyword(Keyword::CURRENT_DATE)
            | TokenKind::Keyword(Keyword::CURRENT_TIMESTAMP) => {
                self.unsupported("CURRENT_TIME/CURRENT_DATE/CURRENT_TIMESTAMP not yet supported")
            }
            _ => self.invalid("expected literal value after DEFAULT"),
        }
    }

    fn table_constraint(&mut self) -> PResult<TableConstraint> {
        if self.eat_kw(Keyword::CONSTRAINT) {
            self.identifier()?;
        }
        if self.eat_kw(Keyword::PRIMARY) {
            self.expect_kw(Keyword::KEY)?;
            let cols = self.indexed_column_list()?;
            self.check_no_conflict_clause()?;
            return Ok(TableConstraint::PrimaryKey(cols));
        }
        if self.eat_kw(Keyword::UNIQUE) {
            let cols = self.indexed_column_list()?;
            self.check_no_conflict_clause()?;
            return Ok(TableConstraint::Unique(cols));
        }
        if self.eat_kw(Keyword::CHECK) {
            self.expect_punct(TokenKind::LParen, "'(' after CHECK")?;
            let expr = self.expr()?;
            self.expect_punct(TokenKind::RParen, "')' to close CHECK")?;
            return Ok(TableConstraint::Check(expr));
        }
        if self.at_kw(Keyword::FOREIGN) {
            return self.unsupported("FOREIGN KEY table constraint not yet supported");
        }
        self.invalid("expected PRIMARY KEY, UNIQUE, CHECK, or FOREIGN KEY table constraint")
    }

    fn indexed_column_list(&mut self) -> PResult<Vec<IndexedColumn>> {
        self.expect_punct(TokenKind::LParen, "'(' after PRIMARY KEY/UNIQUE")?;
        let mut cols = vec![self.indexed_column()?];
        while self.eat_punct(&TokenKind::Comma) {
            cols.push(self.indexed_column()?);
        }
        self.expect_punct(TokenKind::RParen, "')' to close column list")?;
        Ok(cols)
    }

    fn indexed_column(&mut self) -> PResult<IndexedColumn> {
        let expr = self.expr()?;
        let desc = if self.eat_kw(Keyword::ASC) {
            Some(false)
        } else if self.eat_kw(Keyword::DESC) {
            Some(true)
        } else {
            None
        };
        Ok(IndexedColumn { expr, desc })
    }

    pub(super) fn parse_create_index_stmt(&mut self) -> PResult<CreateIndex> {
        let start = self.expect_kw(Keyword::CREATE)?;
        let unique = self.eat_kw(Keyword::UNIQUE);
        self.expect_kw(Keyword::INDEX)?;
        let if_not_exists = self.opt_if_not_exists()?;
        let (name, _) = self.identifier()?;
        self.check_no_schema_qualifier()?;
        self.expect_kw(Keyword::ON)?;
        let (table, _) = self.identifier()?;
        self.check_no_schema_qualifier()?;
        let columns = self.indexed_column_list()?;
        let mut end = self
            .tokens
            .get(self.pos.saturating_sub(1))
            .map_or(start, |t| t.span);
        let where_clause = if self.eat_kw(Keyword::WHERE) {
            let expr = self.expr()?;
            end = expr.span;
            Some(expr)
        } else {
            None
        };
        Ok(CreateIndex {
            unique,
            if_not_exists,
            name,
            table,
            columns,
            where_clause,
            span: join_span(start, end),
        })
    }

    pub(super) fn parse_drop_table_stmt(&mut self) -> PResult<DropTable> {
        let start = self.expect_kw(Keyword::DROP)?;
        self.expect_kw(Keyword::TABLE)?;
        let if_exists = self.opt_if_exists()?;
        let (name, end) = self.identifier()?;
        self.check_no_schema_qualifier()?;
        Ok(DropTable {
            if_exists,
            name,
            span: join_span(start, end),
        })
    }

    pub(super) fn parse_drop_index_stmt(&mut self) -> PResult<DropIndex> {
        let start = self.expect_kw(Keyword::DROP)?;
        self.expect_kw(Keyword::INDEX)?;
        let if_exists = self.opt_if_exists()?;
        let (name, end) = self.identifier()?;
        self.check_no_schema_qualifier()?;
        Ok(DropIndex {
            if_exists,
            name,
            span: join_span(start, end),
        })
    }

    /// `explain-stmt` (#243, grammar V4): `EXPLAIN [QUERY PLAN]
    /// select-stmt`. Only a `SELECT` body is supported — wrapping any
    /// other statement kind (or bare `EXPLAIN` with no `QUERY PLAN`, the
    /// oracle's raw-opcode-dump mode already served by the CLI's
    /// `-explain` flag) is `Unsupported` rather than silently accepted.
    pub(super) fn parse_explain_stmt(&mut self) -> PResult<Explain> {
        self.expect_kw(Keyword::EXPLAIN)?;
        let query_plan = if self.eat_kw(Keyword::QUERY) {
            self.expect_kw(Keyword::PLAN)?;
            true
        } else {
            false
        };
        if !query_plan {
            return self.unsupported(
                "bare EXPLAIN (opcode dump) not supported here — use the CLI's -explain flag",
            );
        }
        let select = self.parse_select_stmt()?;
        Ok(Explain {
            query_plan,
            select: Box::new(select),
        })
    }

    pub(super) fn parse_select_stmt(&mut self) -> PResult<Select> {
        if self.at_kw(Keyword::WITH) {
            return self.unsupported("WITH / CTEs not yet supported");
        }
        if self.at_kw(Keyword::VALUES) {
            return self.unsupported("bare VALUES not yet supported");
        }
        let start = self.expect_kw(Keyword::SELECT)?;

        let distinct = if self.eat_kw(Keyword::DISTINCT) {
            Some(Distinctness::Distinct)
        } else if self.eat_kw(Keyword::ALL) {
            Some(Distinctness::All)
        } else {
            None
        };

        let mut columns = vec![self.result_column()?];
        while self.eat_punct(&TokenKind::Comma) {
            columns.push(self.result_column()?);
        }

        let from = if self.eat_kw(Keyword::FROM) {
            Some(self.parse_from_clause()?)
        } else {
            None
        };

        if self.at_kw(Keyword::WINDOW) {
            return self.unsupported("WINDOW clause not yet supported");
        }

        let where_clause = if self.eat_kw(Keyword::WHERE) {
            Some(self.expr()?)
        } else {
            None
        };

        let mut group_by = Vec::new();
        let mut having = None;
        if self.eat_kw(Keyword::GROUP) {
            self.expect_kw(Keyword::BY)?;
            group_by.push(self.expr()?);
            while self.eat_punct(&TokenKind::Comma) {
                group_by.push(self.expr()?);
            }
            if self.eat_kw(Keyword::HAVING) {
                having = Some(self.expr()?);
            }
        } else if self.at_kw(Keyword::HAVING) {
            return self.unsupported("HAVING without GROUP BY not yet supported");
        }

        let mut compound = Vec::new();
        loop {
            if self.at_kw(Keyword::INTERSECT) || self.at_kw(Keyword::EXCEPT) {
                return self.unsupported("compound SELECT (INTERSECT/EXCEPT) not yet supported");
            }
            if !self.at_kw(Keyword::UNION) {
                break;
            }
            let union_start = self.advance().span;
            if !self.eat_kw(Keyword::ALL) {
                return self.unsupported(
                    "compound SELECT (plain UNION, with dedup) not yet supported; use UNION ALL",
                );
            }
            compound.push(self.parse_compound_select_arm(union_start)?);
        }

        let mut order_by = Vec::new();
        if self.eat_kw(Keyword::ORDER) {
            self.expect_kw(Keyword::BY)?;
            order_by.push(self.ordering_term()?);
            while self.eat_punct(&TokenKind::Comma) {
                order_by.push(self.ordering_term()?);
            }
        }

        let limit = if self.eat_kw(Keyword::LIMIT) {
            let limit_expr = self.expr()?;
            let offset = if self.eat_kw(Keyword::OFFSET) || self.eat_punct(&TokenKind::Comma) {
                Some(self.expr()?)
            } else {
                None
            };
            Some(Limit {
                limit: limit_expr,
                offset,
            })
        } else {
            None
        };

        let end = self
            .tokens
            .get(self.pos.saturating_sub(1))
            .map_or(start, |t| t.span);
        Ok(Select {
            distinct,
            columns,
            from,
            where_clause,
            group_by,
            having,
            compound,
            order_by,
            limit,
            span: join_span(start, end),
        })
    }

    /// One `UNION ALL SELECT ...` arm (#240): same core shape as
    /// [`Self::parse_select_stmt`] minus ORDER BY/LIMIT, which bind to
    /// the whole compound statement rather than any one arm.
    fn parse_compound_select_arm(&mut self, union_start: Span) -> PResult<CompoundSelect> {
        if self.at_kw(Keyword::VALUES) {
            return self.unsupported("UNION ALL VALUES (...) not yet supported");
        }
        let start = self.expect_kw(Keyword::SELECT)?;

        let distinct = if self.eat_kw(Keyword::DISTINCT) {
            Some(Distinctness::Distinct)
        } else if self.eat_kw(Keyword::ALL) {
            Some(Distinctness::All)
        } else {
            None
        };

        let mut columns = vec![self.result_column()?];
        while self.eat_punct(&TokenKind::Comma) {
            columns.push(self.result_column()?);
        }

        let from = if self.eat_kw(Keyword::FROM) {
            Some(self.parse_from_clause()?)
        } else {
            None
        };

        if self.at_kw(Keyword::WINDOW) {
            return self.unsupported("WINDOW clause not yet supported");
        }

        let where_clause = if self.eat_kw(Keyword::WHERE) {
            Some(self.expr()?)
        } else {
            None
        };

        let mut group_by = Vec::new();
        let mut having = None;
        if self.eat_kw(Keyword::GROUP) {
            self.expect_kw(Keyword::BY)?;
            group_by.push(self.expr()?);
            while self.eat_punct(&TokenKind::Comma) {
                group_by.push(self.expr()?);
            }
            if self.eat_kw(Keyword::HAVING) {
                having = Some(self.expr()?);
            }
        } else if self.at_kw(Keyword::HAVING) {
            return self.unsupported("HAVING without GROUP BY not yet supported");
        }

        let end = self
            .tokens
            .get(self.pos.saturating_sub(1))
            .map_or(start, |t| t.span);
        Ok(CompoundSelect {
            op: CompoundOp::UnionAll,
            distinct,
            columns,
            from,
            where_clause,
            group_by,
            having,
            span: join_span(union_start, end),
        })
    }

    fn result_column(&mut self) -> PResult<ResultColumn> {
        if self.eat_punct(&TokenKind::Star) {
            return Ok(ResultColumn::Star);
        }
        // `table-name "." "*"` needs 2-token lookahead to distinguish
        // from a column-ref expression.
        if let TokenKind::Identifier(name) = self.peek().kind.clone() {
            if matches!(self.peek_at(1).kind, TokenKind::Dot)
                && matches!(self.peek_at(2).kind, TokenKind::Star)
            {
                self.advance();
                self.advance();
                self.advance();
                return Ok(ResultColumn::TableStar { table: name });
            }
        }
        let expr = self.expr()?;
        let alias = self.opt_alias()?;
        Ok(ResultColumn::Expr { expr, alias })
    }

    /// `[ [ "AS" ] identifier ]` — a bare identifier only counts as an
    /// alias, never a keyword (keywords are never `TokenKind::Identifier`).
    fn opt_alias(&mut self) -> PResult<Option<String>> {
        if self.eat_kw(Keyword::AS) {
            let (name, _) = self.identifier()?;
            return Ok(Some(name));
        }
        if let TokenKind::Identifier(name) = self.peek().kind.clone() {
            self.advance();
            return Ok(Some(name));
        }
        Ok(None)
    }

    /// Parses `FROM <table_ref> (<join_op> <table_ref> [ON <expr>])*`
    /// (#237, the V4 join slice): an INNER/plain `JOIN`, `LEFT [OUTER]
    /// JOIN`, or `CROSS JOIN` chain, left-to-right. `NATURAL`/`RIGHT`/
    /// `FULL`, `USING (...)`, and comma-style `FROM a, b` are still
    /// explicit `unsupported(..)` errors rather than silently
    /// mis-parsed.
    fn parse_from_clause(&mut self) -> PResult<FromClause> {
        let first = self.table_ref()?;
        let mut joins = Vec::new();
        loop {
            if matches!(self.peek().kind, TokenKind::Comma) {
                return self.unsupported("comma-style JOIN (FROM a, b) not yet supported");
            }
            if self.at_kw(Keyword::NATURAL) {
                return self.unsupported("NATURAL joins not yet supported");
            }
            if self.at_kw(Keyword::RIGHT) {
                return self.unsupported("RIGHT joins not yet supported");
            }
            if self.at_kw(Keyword::FULL) {
                return self.unsupported("FULL joins not yet supported");
            }
            // A bare `OUTER` only ever appears right after `LEFT`/
            // `RIGHT`/`FULL` (consumed together with those, below) —
            // seeing it here means some other/malformed join-operator
            // ordering. Reporting it as unsupported keeps it out of the
            // "unexpected trailing token" hard-error bucket, matching
            // this parser's convention of a graceful `unsupported(..)`
            // for anything recognizably join-shaped but out of this
            // slice's scope.
            if self.at_kw(Keyword::OUTER) {
                return self
                    .unsupported("OUTER without a preceding LEFT/RIGHT/FULL not yet supported");
            }
            if self.eat_kw(Keyword::CROSS) {
                self.expect_kw(Keyword::JOIN)?;
                let table = self.table_ref()?;
                if self.at_kw(Keyword::ON) {
                    return self.unsupported("CROSS JOIN with an ON clause not yet supported");
                }
                if self.at_kw(Keyword::USING) {
                    return self.unsupported("USING clause not yet supported");
                }
                joins.push(Join {
                    op: JoinOp::Cross,
                    table,
                    constraint: None,
                });
                continue;
            }
            let op = if self.eat_kw(Keyword::LEFT) {
                self.eat_kw(Keyword::OUTER);
                self.expect_kw(Keyword::JOIN)?;
                Some(JoinOp::Left)
            } else if self.eat_kw(Keyword::INNER) {
                self.expect_kw(Keyword::JOIN)?;
                Some(JoinOp::Inner)
            } else if self.eat_kw(Keyword::JOIN) {
                Some(JoinOp::Inner)
            } else {
                None
            };
            let Some(op) = op else { break };
            let table = self.table_ref()?;
            if self.at_kw(Keyword::USING) {
                return self.unsupported("USING clause not yet supported");
            }
            // A real `JOIN`/`INNER JOIN`/`LEFT [OUTER] JOIN` with no
            // `ON`/`USING` at all is valid SQL (equivalent to a
            // constraint-less cross join) — real SQLite accepts it, so
            // this stays a graceful `unsupported(..)` rather than the
            // hard parse error `expect_kw` would raise, which would
            // otherwise misclassify valid SQL as malformed (caught by
            // `tests/corpus/extracted_sql_test.rs`'s
            // `no_extracted_select_is_reported_invalid`). This bounded
            // MVP only compiles the `ON`-qualified form.
            if !self.at_kw(Keyword::ON) {
                return self.unsupported("JOIN without an ON/USING clause not yet supported");
            }
            self.expect_kw(Keyword::ON)?;
            let on_expr = self.expr()?;
            joins.push(Join {
                op,
                table,
                constraint: Some(JoinConstraint::On(on_expr)),
            });
        }
        Ok(FromClause { first, joins })
    }

    /// A single `table-name [AS alias]` — shared by the FROM clause's
    /// first table and every join's right-hand table. Schema-qualified
    /// names, subqueries, table-valued functions, and `INDEXED BY`/`NOT
    /// INDEXED` stay explicit `unsupported(..)` errors.
    fn table_ref(&mut self) -> PResult<TableRef> {
        if matches!(self.peek().kind, TokenKind::LParen) {
            return self
                .unsupported("table-valued functions / subqueries in FROM not yet supported");
        }
        let (name, start) = self.identifier()?;
        if matches!(self.peek().kind, TokenKind::Dot) {
            return self.unsupported("schema-qualified table names not yet supported");
        }
        let alias = self.opt_alias()?;
        let end = alias.is_some();
        let span = if end {
            join_span(
                start,
                self.tokens
                    .get(self.pos.saturating_sub(1))
                    .map_or(start, |t| t.span),
            )
        } else {
            start
        };

        if self.at_kw(Keyword::INDEXED) {
            return self.unsupported("INDEXED BY not yet supported");
        }
        if self.at_kw(Keyword::NOT)
            && matches!(self.peek_at(1).kind, TokenKind::Keyword(Keyword::INDEXED))
        {
            return self.unsupported("NOT INDEXED not yet supported");
        }
        if matches!(self.peek().kind, TokenKind::LParen) {
            return self
                .unsupported("table-valued functions / subqueries in FROM not yet supported");
        }

        Ok(TableRef { name, alias, span })
    }

    fn ordering_term(&mut self) -> PResult<OrderingTerm> {
        let expr = self.expr()?;
        let desc = if self.eat_kw(Keyword::ASC) {
            Some(false)
        } else if self.eat_kw(Keyword::DESC) {
            Some(true)
        } else {
            None
        };
        let nulls_last = if self.eat_kw(Keyword::NULLS) {
            if self.eat_kw(Keyword::FIRST) {
                Some(false)
            } else if self.eat_kw(Keyword::LAST) {
                Some(true)
            } else {
                return self.invalid("expected FIRST or LAST after NULLS");
            }
        } else {
            None
        };
        Ok(OrderingTerm {
            expr,
            desc,
            nulls_last,
        })
    }

    // ---- expressions -----------------------------------------------------

    pub(super) fn expr(&mut self) -> PResult<Expr> {
        self.with_depth_guard(|this| this.bool_expr(0))
    }

    /// Precedence climb over `OR` (prec 0) and `AND` (prec 1, binds
    /// tighter) — one stack frame here instead of two separate
    /// pass-through functions (`or_expr`/`and_expr`) per nesting level.
    /// Collapsing these was one part of narrowing the debug/release stack
    /// gap that let a stack overflow pre-empt the `MAX_EXPR_DEPTH` guard
    /// (#118); see `binary_expr` for the larger half of that collapse.
    fn bool_expr(&mut self, min_prec: u8) -> PResult<Expr> {
        let mut lhs = self.not_expr()?;
        loop {
            // AND binds tighter than OR, so AND's rhs only ever needs
            // `not_expr` (nothing tighter exists in this pair); OR's rhs
            // must still climb through any following AND, but never a
            // sibling OR (left-associative).
            lhs = if self.at_kw(Keyword::AND) {
                if min_prec > 1 {
                    break;
                }
                self.advance();
                bin(BinaryOp::And, lhs, self.not_expr()?)
            } else if self.at_kw(Keyword::OR) {
                if min_prec > 0 {
                    break;
                }
                self.advance();
                bin(BinaryOp::Or, lhs, self.bool_expr(1)?)
            } else {
                break;
            };
        }
        Ok(lhs)
    }

    fn not_expr(&mut self) -> PResult<Expr> {
        self.with_depth_guard(|this| {
            if this.at_kw(Keyword::NOT) {
                let start = this.advance().span;
                if this.at_kw(Keyword::EXISTS) {
                    this.advance();
                    return this.exists_tail(start, true);
                }
                let inner = this.not_expr()?;
                let span = join_span(start, inner.span);
                return Ok(Expr {
                    kind: ExprKind::Unary {
                        op: UnaryOp::Not,
                        expr: Box::new(inner),
                    },
                    span,
                });
            }
            this.equality_expr()
        })
    }

    fn equality_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.binary_expr(1)?;
        loop {
            lhs = match self.peek().kind.clone() {
                TokenKind::Eq => {
                    self.advance();
                    let rhs = self.binary_expr(1)?;
                    bin(BinaryOp::Eq, lhs, rhs)
                }
                TokenKind::Ne => {
                    self.advance();
                    let rhs = self.binary_expr(1)?;
                    bin(BinaryOp::Ne, lhs, rhs)
                }
                TokenKind::Keyword(Keyword::IS) => {
                    self.advance();
                    let negated = self.eat_kw(Keyword::NOT);
                    let rhs = self.binary_expr(1)?;
                    let span = join_span(lhs.span, rhs.span);
                    Expr {
                        kind: ExprKind::Is {
                            lhs: Box::new(lhs),
                            rhs: Box::new(rhs),
                            negated,
                        },
                        span,
                    }
                }
                TokenKind::Keyword(Keyword::ISNULL) => {
                    let end = self.advance().span;
                    let span = join_span(lhs.span, end);
                    Expr {
                        kind: ExprKind::IsNull {
                            expr: Box::new(lhs),
                            negated: false,
                        },
                        span,
                    }
                }
                TokenKind::Keyword(Keyword::NOTNULL) => {
                    let end = self.advance().span;
                    let span = join_span(lhs.span, end);
                    Expr {
                        kind: ExprKind::IsNull {
                            expr: Box::new(lhs),
                            negated: true,
                        },
                        span,
                    }
                }
                TokenKind::Keyword(Keyword::BETWEEN) => {
                    self.advance();
                    let (lo, hi) = self.between_tail()?;
                    let span = join_span(lhs.span, hi.span);
                    Expr {
                        kind: ExprKind::Between {
                            expr: Box::new(lhs),
                            lo: Box::new(lo),
                            hi: Box::new(hi),
                            negated: false,
                        },
                        span,
                    }
                }
                TokenKind::Keyword(Keyword::IN) => {
                    self.advance();
                    self.in_tail(lhs, false)?
                }
                TokenKind::Keyword(Keyword::LIKE) | TokenKind::Keyword(Keyword::GLOB) => {
                    let glob = self.at_kw(Keyword::GLOB);
                    self.advance();
                    self.like_tail(lhs, glob, false)?
                }
                TokenKind::Keyword(Keyword::NOT) => match self.peek_at(1).kind.clone() {
                    TokenKind::Null => {
                        self.advance();
                        let end = self.advance().span;
                        let span = join_span(lhs.span, end);
                        Expr {
                            kind: ExprKind::IsNull {
                                expr: Box::new(lhs),
                                negated: true,
                            },
                            span,
                        }
                    }
                    TokenKind::Keyword(Keyword::BETWEEN) => {
                        self.advance();
                        self.advance();
                        let (lo, hi) = self.between_tail()?;
                        let span = join_span(lhs.span, hi.span);
                        Expr {
                            kind: ExprKind::Between {
                                expr: Box::new(lhs),
                                lo: Box::new(lo),
                                hi: Box::new(hi),
                                negated: true,
                            },
                            span,
                        }
                    }
                    TokenKind::Keyword(Keyword::IN) => {
                        self.advance();
                        self.advance();
                        self.in_tail(lhs, true)?
                    }
                    TokenKind::Keyword(Keyword::LIKE) | TokenKind::Keyword(Keyword::GLOB) => {
                        let glob =
                            matches!(self.peek_at(1).kind, TokenKind::Keyword(Keyword::GLOB));
                        self.advance();
                        self.advance();
                        self.like_tail(lhs, glob, true)?
                    }
                    _ => break,
                },
                _ => break,
            };
        }
        Ok(lhs)
    }

    fn between_tail(&mut self) -> PResult<(Expr, Expr)> {
        let lo = self.binary_expr(1)?;
        self.expect_kw(Keyword::AND)?;
        let hi = self.binary_expr(1)?;
        Ok((lo, hi))
    }

    /// `EXISTS (SELECT ...)` / `NOT EXISTS (SELECT ...)` — `start` is the
    /// span of the `EXISTS`/`NOT` token this tail follows, and anything
    /// after `EXISTS (` that isn't a `SELECT` is still `unsupported`
    /// (subqueries in FROM, `ANY`/`ALL`/`SOME`, etc. all parse a `SELECT`
    /// here so this stays narrow).
    fn exists_tail(&mut self, start: Span, negated: bool) -> PResult<Expr> {
        self.expect_punct(TokenKind::LParen, "'(' after EXISTS")?;
        if !self.at_kw(Keyword::SELECT) {
            return self.unsupported("EXISTS ( ... ) requires a SELECT subquery");
        }
        let subquery = self.parse_select_stmt()?;
        if matches!(
            self.peek().kind,
            TokenKind::Keyword(Keyword::UNION)
                | TokenKind::Keyword(Keyword::INTERSECT)
                | TokenKind::Keyword(Keyword::EXCEPT)
        ) {
            return self.unsupported("compound SELECT (UNION/INTERSECT/EXCEPT) not yet supported");
        }
        let end = self.expect_punct(TokenKind::RParen, "')' to close EXISTS subquery")?;
        let span = join_span(start, end);
        Ok(Expr {
            kind: ExprKind::Exists {
                subquery: Box::new(subquery),
                negated,
            },
            span,
        })
    }

    fn in_tail(&mut self, lhs: Expr, negated: bool) -> PResult<Expr> {
        if !matches!(self.peek().kind, TokenKind::LParen) {
            return self.unsupported("IN <table-name> not yet supported");
        }
        self.expect_punct(TokenKind::LParen, "'(' after IN")?;
        if self.at_kw(Keyword::SELECT) {
            let subquery = self.parse_select_stmt()?;
            if matches!(
                self.peek().kind,
                TokenKind::Keyword(Keyword::UNION)
                    | TokenKind::Keyword(Keyword::INTERSECT)
                    | TokenKind::Keyword(Keyword::EXCEPT)
            ) {
                return self
                    .unsupported("compound SELECT (UNION/INTERSECT/EXCEPT) not yet supported");
            }
            let end = self.expect_punct(TokenKind::RParen, "')' to close IN subquery")?;
            let span = join_span(lhs.span, end);
            return Ok(Expr {
                kind: ExprKind::InSubquery {
                    expr: Box::new(lhs),
                    subquery: Box::new(subquery),
                    negated,
                },
                span,
            });
        }
        let list = if matches!(self.peek().kind, TokenKind::RParen) {
            Vec::new()
        } else {
            self.expr_list()?
        };
        let end = self.expect_punct(TokenKind::RParen, "')' to close IN list")?;
        let span = join_span(lhs.span, end);
        Ok(Expr {
            kind: ExprKind::In {
                expr: Box::new(lhs),
                list,
                negated,
            },
            span,
        })
    }

    fn like_tail(&mut self, lhs: Expr, glob: bool, negated: bool) -> PResult<Expr> {
        let pattern = self.binary_expr(1)?;
        let mut span = join_span(lhs.span, pattern.span);
        let escape = if self.eat_kw(Keyword::ESCAPE) {
            let e = self.binary_expr(1)?;
            span = join_span(span, e.span);
            Some(Box::new(e))
        } else {
            None
        };
        Ok(Expr {
            kind: ExprKind::Like {
                expr: Box::new(lhs),
                pattern: Box::new(pattern),
                glob,
                negated,
                escape,
            },
            span,
        })
    }

    /// Precedence climb merging what used to be four separate pass-through
    /// levels — `relational_expr` (prec 1: `<`/`<=`/`>`/`>=`) ->
    /// `bitwise_expr` (prec 2: `&`/`|`/`<<`/`>>`) -> `additive_expr` (prec
    /// 3: `+`/`-`) -> `multiplicative_expr` (prec 4: `*`/`/`/`%`) ->
    /// `concat_expr` (prec 5: `||`) — into one stack frame per nesting
    /// level instead of five. All five operator groups are left-
    /// associative, so a run of same-precedence operators (`1+2+3+...`)
    /// stays iterative (the `loop`); recursion only climbs one level per
    /// *distinct* precedence step in the expression, exactly mirroring the
    /// original call chain's shape. `min_prec` is the lowest precedence
    /// this call is willing to consume; callers needing the full chain
    /// (what `relational_expr` used to mean) pass `1`. Narrows the debug
    /// stack-depth gap that let a stack overflow pre-empt the
    /// `MAX_EXPR_DEPTH` guard (#118).
    fn binary_expr(&mut self, min_prec: u8) -> PResult<Expr> {
        let mut lhs = self.arrow_expr()?;
        while let Some((op, prec)) = Self::binary_op(&self.peek().kind) {
            if prec < min_prec {
                break;
            }
            self.advance();
            let rhs = self.binary_expr(prec.saturating_add(1))?;
            lhs = bin(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn binary_op(kind: &TokenKind) -> Option<(BinaryOp, u8)> {
        Some(match kind {
            TokenKind::Lt => (BinaryOp::Lt, 1),
            TokenKind::Le => (BinaryOp::Le, 1),
            TokenKind::Gt => (BinaryOp::Gt, 1),
            TokenKind::Ge => (BinaryOp::Ge, 1),
            TokenKind::BitAnd => (BinaryOp::BitAnd, 2),
            TokenKind::BitOr => (BinaryOp::BitOr, 2),
            TokenKind::Shl => (BinaryOp::Shl, 2),
            TokenKind::Shr => (BinaryOp::Shr, 2),
            TokenKind::Plus => (BinaryOp::Add, 3),
            TokenKind::Minus => (BinaryOp::Sub, 3),
            TokenKind::Star => (BinaryOp::Mul, 4),
            TokenKind::Slash => (BinaryOp::Div, 4),
            TokenKind::Percent => (BinaryOp::Mod, 4),
            TokenKind::Concat => (BinaryOp::Concat, 5),
            _ => return None,
        })
    }

    /// `->` / `->>` (JSON extract operators, V11) are recognized here so
    /// they're reported `Unsupported` rather than falling through to a
    /// generic "unexpected trailing token" `Invalid`.
    fn arrow_expr(&mut self) -> PResult<Expr> {
        let lhs = self.collate_expr()?;
        if matches!(self.peek().kind, TokenKind::Arrow | TokenKind::ArrowArrow) {
            return self.unsupported("-> / ->> operators not yet supported");
        }
        Ok(lhs)
    }

    fn collate_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.unary_expr()?;
        while self.eat_kw(Keyword::COLLATE) {
            let (name, end) = self.identifier()?;
            let span = join_span(lhs.span, end);
            lhs = Expr {
                kind: ExprKind::Collate {
                    expr: Box::new(lhs),
                    collation: name,
                },
                span,
            };
        }
        Ok(lhs)
    }

    fn unary_expr(&mut self) -> PResult<Expr> {
        self.with_depth_guard(|this| {
            let op = match this.peek().kind {
                TokenKind::Plus => Some(UnaryOp::Plus),
                TokenKind::Minus => Some(UnaryOp::Minus),
                TokenKind::BitNot => Some(UnaryOp::BitNot),
                _ => None,
            };
            if let Some(op) = op {
                let start = this.advance().span;
                let inner = this.unary_expr()?;
                let span = join_span(start, inner.span);
                // `9223372036854775808` has no positive i64
                // representation, so the tokenizer folds it to a Float.
                // Negated, it is exactly i64::MIN — SQLite parses
                // `-9223372036854775808` as an INTEGER literal, not a
                // REAL (spike #59 finding).
                if matches!(op, UnaryOp::Minus) {
                    if let ExprKind::Literal(Literal::Float(f)) = inner.kind {
                        if f == 9_223_372_036_854_775_808.0 {
                            return Ok(Expr {
                                kind: ExprKind::Literal(Literal::Integer(i64::MIN)),
                                span,
                            });
                        }
                    }
                }
                return Ok(Expr {
                    kind: ExprKind::Unary {
                        op,
                        expr: Box::new(inner),
                    },
                    span,
                });
            }
            this.primary_expr()
        })
    }

    fn primary_expr(&mut self) -> PResult<Expr> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Integer(v) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::Integer(v)),
                    span: tok.span,
                })
            }
            TokenKind::Float(v) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::Float(v)),
                    span: tok.span,
                })
            }
            TokenKind::String(s) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::Str(s)),
                    span: tok.span,
                })
            }
            TokenKind::Blob(b) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::Blob(b)),
                    span: tok.span,
                })
            }
            TokenKind::Null => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::Null),
                    span: tok.span,
                })
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::True),
                    span: tok.span,
                })
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Literal(Literal::False),
                    span: tok.span,
                })
            }
            TokenKind::Param(p) => {
                self.advance();
                let kind = match p {
                    Param::Anonymous => ParamKind::Anonymous,
                    Param::Numbered(n) => ParamKind::Numbered(n),
                    Param::Colon(s) => ParamKind::Colon(s),
                    Param::At(s) => ParamKind::At(s),
                    Param::Dollar(s) => ParamKind::Dollar(s),
                };
                Ok(Expr {
                    kind: ExprKind::Param(kind),
                    span: tok.span,
                })
            }
            TokenKind::Keyword(Keyword::CURRENT_TIME)
            | TokenKind::Keyword(Keyword::CURRENT_DATE)
            | TokenKind::Keyword(Keyword::CURRENT_TIMESTAMP) => {
                self.unsupported("CURRENT_TIME/CURRENT_DATE/CURRENT_TIMESTAMP not yet supported")
            }
            TokenKind::Keyword(Keyword::CASE) => self.case_expr(),
            TokenKind::Keyword(Keyword::CAST) => self.cast_expr(),
            TokenKind::Keyword(Keyword::EXISTS) => {
                let start = tok.span;
                self.advance();
                self.exists_tail(start, false)
            }
            TokenKind::Identifier(name) => {
                self.advance();
                if matches!(self.peek().kind, TokenKind::LParen) {
                    return self.function_call(name, tok.span);
                }
                let mut parts = vec![name];
                while matches!(self.peek().kind, TokenKind::Dot) && parts.len() < 3 {
                    self.advance();
                    let (part, _) = self.identifier()?;
                    parts.push(part);
                }
                let end = self
                    .tokens
                    .get(self.pos.saturating_sub(1))
                    .map_or(tok.span, |t| t.span);
                let span = join_span(tok.span, end);
                let kind = match parts.len() {
                    1 => ExprKind::Column {
                        table: None,
                        catalog: None,
                        name: parts.remove(0),
                    },
                    2 => ExprKind::Column {
                        catalog: None,
                        table: Some(parts.remove(0)),
                        name: parts.remove(0),
                    },
                    _ => ExprKind::Column {
                        catalog: Some(parts.remove(0)),
                        table: Some(parts.remove(0)),
                        name: parts.remove(0),
                    },
                };
                Ok(Expr { kind, span })
            }
            // SQLite treats most keywords as usable function names when
            // followed by `(` (e.g. `replace(...)`, `glob(...)`) — only
            // the handful matched above (CASE/CAST/EXISTS/CURRENT_*)
            // are true reserved words in expression position.
            TokenKind::Keyword(kw) if matches!(self.peek_at(1).kind, TokenKind::LParen) => {
                self.advance();
                self.function_call(format!("{kw:?}"), tok.span)
            }
            TokenKind::LParen => {
                self.advance();
                if self.at_kw(Keyword::SELECT) {
                    let subquery = self.parse_select_stmt()?;
                    if matches!(
                        self.peek().kind,
                        TokenKind::Keyword(Keyword::UNION)
                            | TokenKind::Keyword(Keyword::INTERSECT)
                            | TokenKind::Keyword(Keyword::EXCEPT)
                    ) {
                        return self.unsupported(
                            "compound SELECT (UNION/INTERSECT/EXCEPT) not yet supported",
                        );
                    }
                    let end = self.expect_punct(TokenKind::RParen, "')' to close subquery")?;
                    let span = join_span(tok.span, end);
                    return Ok(Expr {
                        kind: ExprKind::Subquery(Box::new(subquery)),
                        span,
                    });
                }
                let inner = self.expr()?;
                let end = self.expect_punct(TokenKind::RParen, "')' to close expression")?;
                let span = join_span(tok.span, end);
                Ok(Expr {
                    kind: ExprKind::Paren(Box::new(inner)),
                    span,
                })
            }
            other => Err(ParseFail::Invalid {
                message: format!("expected column or expression, found {other:?}"),
                span: tok.span,
            }),
        }
    }

    fn function_call(&mut self, name: String, start: Span) -> PResult<Expr> {
        self.expect_punct(TokenKind::LParen, "'(' after function name")?;
        let distinct = self.eat_kw(Keyword::DISTINCT);
        let args = if self.eat_punct(&TokenKind::Star) {
            FunctionArgs::Star
        } else if matches!(self.peek().kind, TokenKind::RParen) {
            FunctionArgs::List(Vec::new())
        } else {
            FunctionArgs::List(self.expr_list()?)
        };
        let mut end = self.expect_punct(TokenKind::RParen, "')' to close function call")?;
        if self.at_kw(Keyword::OVER) || self.at_kw(Keyword::FILTER) {
            return self.unsupported("window functions (OVER/FILTER) not yet supported");
        }
        let span = join_span(start, {
            end.len = end.len.max(1);
            end
        });
        Ok(Expr {
            kind: ExprKind::FunctionCall {
                name,
                distinct,
                args,
            },
            span,
        })
    }

    fn case_expr(&mut self) -> PResult<Expr> {
        let start = self.advance().span; // CASE
        let operand = if self.at_kw(Keyword::WHEN) {
            None
        } else {
            Some(Box::new(self.expr()?))
        };
        let mut whens = Vec::new();
        while self.eat_kw(Keyword::WHEN) {
            let cond = self.expr()?;
            self.expect_kw(Keyword::THEN)?;
            let res = self.expr()?;
            whens.push((cond, res));
        }
        if whens.is_empty() {
            return self.invalid("expected WHEN in CASE expression");
        }
        let else_ = if self.eat_kw(Keyword::ELSE) {
            Some(Box::new(self.expr()?))
        } else {
            None
        };
        let end = self.expect_kw(Keyword::END)?;
        Ok(Expr {
            kind: ExprKind::Case {
                operand,
                whens,
                else_,
            },
            span: join_span(start, end),
        })
    }

    fn cast_expr(&mut self) -> PResult<Expr> {
        let start = self.advance().span; // CAST
        self.expect_punct(TokenKind::LParen, "'(' after CAST")?;
        let inner = self.expr()?;
        self.expect_kw(Keyword::AS)?;
        let type_name = self.type_name()?;
        let end = self.expect_punct(TokenKind::RParen, "')' to close CAST")?;
        Ok(Expr {
            kind: ExprKind::Cast {
                expr: Box::new(inner),
                type_name,
            },
            span: join_span(start, end),
        })
    }

    /// `type-name ::= identifier { identifier } [ "(" NUMBER [ "," NUMBER ] ")" ]`
    fn type_name(&mut self) -> PResult<String> {
        let (first, _) = self.identifier()?;
        let mut parts = vec![first];
        while let TokenKind::Identifier(_) = self.peek().kind {
            let (part, _) = self.identifier()?;
            parts.push(part);
        }
        let mut name = parts.join(" ");
        if self.eat_punct(&TokenKind::LParen) {
            let n1 = self.number_literal()?;
            name.push('(');
            name.push_str(&n1);
            if self.eat_punct(&TokenKind::Comma) {
                let n2 = self.number_literal()?;
                name.push_str(", ");
                name.push_str(&n2);
            }
            self.expect_punct(TokenKind::RParen, "')' to close type size")?;
            name.push(')');
        }
        Ok(name)
    }

    fn number_literal(&mut self) -> PResult<String> {
        match self.peek().kind.clone() {
            TokenKind::Integer(v) => {
                self.advance();
                Ok(v.to_string())
            }
            other => {
                let span = self.peek().span;
                Err(ParseFail::Invalid {
                    message: format!("expected number, found {other:?}"),
                    span,
                })
            }
        }
    }

    fn expr_list(&mut self) -> PResult<Vec<Expr>> {
        let mut list = vec![self.expr()?];
        while self.eat_punct(&TokenKind::Comma) {
            list.push(self.expr()?);
        }
        Ok(list)
    }
}

fn bin(op: BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
    let span = join_span(lhs.span, rhs.span);
    Expr {
        kind: ExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        span,
    }
}
