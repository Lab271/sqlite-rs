//! The three-way SELECT-core parse outcome (spec 002-parser
//! Requirement 4), per spike 006 (#57)'s verdict: `sqlite3` only ever
//! accepts or rejects, but sqlite-rs's V2 slice additionally needs to
//! distinguish "syntactically valid SQL we haven't implemented yet"
//! (e.g. `JOIN`) from "actually malformed" — otherwise a not-yet-built
//! feature reads identically to a typo.

use super::ast::{Insert, Select};
use super::grammar::Parser;
use super::tokenizer::{Span, Tokenizer};

#[derive(Debug, Clone, PartialEq)]
pub enum ParseOutcome {
    /// Parsed successfully into a [`Select`].
    Accepted(Box<Select>),
    /// Syntactically-recognized SQL this parser doesn't implement yet
    /// (joins, subqueries, compound selects, ...). `span` points at the
    /// token that triggered the unsupported construct.
    Unsupported { message: String, span: Span },
    /// Malformed SQL: a genuine syntax error. `span` points at the
    /// offending token.
    Invalid { message: String, span: Span },
}

/// Failure carried internally by the recursive-descent parser; folded
/// into [`ParseOutcome::Unsupported`]/[`ParseOutcome::Invalid`] by
/// [`parse_select`].
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ParseFail {
    Unsupported { message: String, span: Span },
    Invalid { message: String, span: Span },
}

pub(super) type PResult<T> = Result<T, ParseFail>;

/// Parses a single SELECT statement (spec 002-parser Requirements 2-4;
/// grammar `.openspec/grammar/sqlite.ebnf` V2 block). Never panics —
/// any input produces one of the three [`ParseOutcome`] variants.
pub fn parse_select(src: &str) -> ParseOutcome {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_select_stmt() {
        Ok(select) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(select)),
            Err(ParseFail::Unsupported { message, span }) => {
                ParseOutcome::Unsupported { message, span }
            }
            Err(ParseFail::Invalid { message, span }) => ParseOutcome::Invalid { message, span },
        },
        Err(ParseFail::Unsupported { message, span }) => {
            ParseOutcome::Unsupported { message, span }
        }
        Err(ParseFail::Invalid { message, span }) => ParseOutcome::Invalid { message, span },
    }
}

/// Same three-way contract as [`ParseOutcome`], for INSERT (spec
/// 002-parser, V3 block).
#[derive(Debug, Clone, PartialEq)]
pub enum InsertOutcome {
    Accepted(Box<Insert>),
    Unsupported { message: String, span: Span },
    Invalid { message: String, span: Span },
}

/// Parses a single INSERT statement (grammar `.openspec/grammar/sqlite.ebnf`
/// V3 block). Never panics — any input produces one of the three
/// [`InsertOutcome`] variants.
pub fn parse_insert(src: &str) -> InsertOutcome {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_insert_stmt() {
        Ok(insert) => match parser.expect_end() {
            Ok(()) => InsertOutcome::Accepted(Box::new(insert)),
            Err(ParseFail::Unsupported { message, span }) => {
                InsertOutcome::Unsupported { message, span }
            }
            Err(ParseFail::Invalid { message, span }) => InsertOutcome::Invalid { message, span },
        },
        Err(ParseFail::Unsupported { message, span }) => {
            InsertOutcome::Unsupported { message, span }
        }
        Err(ParseFail::Invalid { message, span }) => InsertOutcome::Invalid { message, span },
    }
}
