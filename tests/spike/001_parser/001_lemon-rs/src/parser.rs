// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Parser front end: wires the hand-rolled tokenizer into the Lemon-generated
//! LALR(1) parser.

pub mod stack;

use crate::ast::Stmt;
use crate::tokenizer::{self, Tok};

/// Side channel between the generated reduce actions and the caller.
///
/// Lemon actions have no return value, so the accepted statement (and any syntax
/// error) is handed back through `%extra_context`. The `'input` lifetime is
/// appended by lemon when it emits the field declaration, hence the borrow of
/// the source text.
pub struct Context<'input> {
    pub input: &'input str,
    pub stmt: Option<Stmt>,
    pub error: Option<String>,
}

impl<'input> Context<'input> {
    pub fn new(input: &'input str) -> Self {
        Context {
            input,
            stmt: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
}

impl ParseError {
    pub fn new(message: impl Into<String>) -> Self {
        ParseError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

// The lemon-generated parser. `TokenType`, `yyParser` and the reduce actions all
// live in here; the grammar is src/parse.y.
#[allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    unused,
    unfulfilled_lint_expectations,
    clippy::all
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/parse.rs"));
}

pub use generated::TokenType;

/// Parse a single SQL statement (no trailing semicolon).
#[allow(non_snake_case)]
pub fn parse(sql: &str) -> Result<Stmt, ParseError> {
    let tokens = tokenizer::tokenize(sql)?;

    let mut parser = generated::yyParser::new(Context::new(sql));
    for (ty, tok) in tokens {
        parser.Parse(ty, tok)?;
        if let Some(message) = parser.ctx.error.take() {
            return Err(ParseError::new(message));
        }
    }
    // Feeding token 0 (EOF) is how a Lemon parser is told the input has ended.
    parser.Parse(
        TokenType::EOF,
        Tok {
            text: "",
            pos: sql.len() as u32,
        },
    )?;
    if let Some(message) = parser.ctx.error.take() {
        return Err(ParseError::new(message));
    }

    let stmt = parser.ctx.stmt.take();
    parser.ParseFinalize();
    stmt.ok_or_else(|| ParseError::new("incomplete input: no statement parsed"))
}
