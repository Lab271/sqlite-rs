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
            Some(self.table_ref()?)
        } else {
            None
        };

        if self.at_kw(Keyword::GROUP) {
            return self.unsupported("GROUP BY not yet supported");
        }
        if self.at_kw(Keyword::HAVING) {
            return self.unsupported("HAVING not yet supported");
        }
        if self.at_kw(Keyword::WINDOW) {
            return self.unsupported("WINDOW clause not yet supported");
        }

        let where_clause = if self.eat_kw(Keyword::WHERE) {
            Some(self.expr()?)
        } else {
            None
        };

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
            order_by,
            limit,
            span: join_span(start, end),
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

        if self.at_kw(Keyword::JOIN)
            || self.at_kw(Keyword::NATURAL)
            || self.at_kw(Keyword::LEFT)
            || self.at_kw(Keyword::RIGHT)
            || self.at_kw(Keyword::FULL)
            || self.at_kw(Keyword::INNER)
            || self.at_kw(Keyword::CROSS)
            || self.at_kw(Keyword::OUTER)
            || self.at_kw(Keyword::INDEXED)
            || matches!(self.peek().kind, TokenKind::Comma)
        {
            return self.unsupported("JOIN / multi-table FROM not yet supported");
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

    fn in_tail(&mut self, lhs: Expr, negated: bool) -> PResult<Expr> {
        if !matches!(self.peek().kind, TokenKind::LParen) {
            return self.unsupported("IN <table-name> not yet supported");
        }
        self.expect_punct(TokenKind::LParen, "'(' after IN")?;
        if self.at_kw(Keyword::SELECT) {
            return self.unsupported("IN (subquery) not yet supported");
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
                self.unsupported("EXISTS (subquery) not yet supported")
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
                    return self.unsupported("subquery expressions not yet supported");
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
