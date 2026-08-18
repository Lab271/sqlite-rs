//! The three-way SELECT-core parse outcome (spec 002-parser
//! Requirement 4), per spike 006 (#57)'s verdict: `sqlite3` only ever
//! accepts or rejects, but sqlite-rs's V2 slice additionally needs to
//! distinguish "syntactically valid SQL we haven't implemented yet"
//! (e.g. `JOIN`) from "actually malformed" — otherwise a not-yet-built
//! feature reads identically to a typo.

use super::ast::{CreateIndex, CreateTable, Delete, DropIndex, DropTable, Insert, Select, Update};
use super::grammar::Parser;
use super::tokenizer::{Span, Tokenizer};

#[derive(Debug, Clone, PartialEq)]
pub enum ParseOutcome<T> {
    /// Parsed successfully into a `T` (e.g. [`Select`], [`Update`]).
    Accepted(Box<T>),
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
pub fn parse_select(src: &str) -> ParseOutcome<Select> {
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

/// Same three-way contract as [`ParseOutcome`], for DELETE (spec
/// 002-parser, V3 block).
#[derive(Debug, Clone, PartialEq)]
pub enum DeleteOutcome {
    Accepted(Box<Delete>),
    Unsupported { message: String, span: Span },
    Invalid { message: String, span: Span },
}

/// Parses a single DELETE statement (grammar `.openspec/grammar/sqlite.ebnf`
/// V3 block). Never panics — any input produces one of the three
/// [`DeleteOutcome`] variants.
pub fn parse_delete(src: &str) -> DeleteOutcome {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_delete_stmt() {
        Ok(delete) => match parser.expect_end() {
            Ok(()) => DeleteOutcome::Accepted(Box::new(delete)),
            Err(ParseFail::Unsupported { message, span }) => {
                DeleteOutcome::Unsupported { message, span }
            }
            Err(ParseFail::Invalid { message, span }) => DeleteOutcome::Invalid { message, span },
        },
        Err(ParseFail::Unsupported { message, span }) => {
            DeleteOutcome::Unsupported { message, span }
        }
        Err(ParseFail::Invalid { message, span }) => DeleteOutcome::Invalid { message, span },
    }
}

/// Parses a single UPDATE statement (spec 002-parser V3 slice; grammar
/// `.openspec/grammar/sqlite.ebnf` `update-stmt`). Never panics — any
/// input produces one of the three [`ParseOutcome`] variants.
pub fn parse_update(src: &str) -> ParseOutcome<Update> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_update_stmt() {
        Ok(update) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(update)),
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

/// Same three-way contract as [`ParseOutcome`], for CREATE TABLE (spec
/// 002-parser, V3 block).
#[derive(Debug, Clone, PartialEq)]
pub enum CreateTableOutcome {
    Accepted(Box<CreateTable>),
    Unsupported { message: String, span: Span },
    Invalid { message: String, span: Span },
}

/// Parses a single CREATE TABLE statement (grammar
/// `.openspec/grammar/sqlite.ebnf` V3 block). Never panics — any input
/// produces one of the three [`CreateTableOutcome`] variants.
pub fn parse_create_table(src: &str) -> CreateTableOutcome {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_create_table_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => CreateTableOutcome::Accepted(Box::new(stmt)),
            Err(ParseFail::Unsupported { message, span }) => {
                CreateTableOutcome::Unsupported { message, span }
            }
            Err(ParseFail::Invalid { message, span }) => {
                CreateTableOutcome::Invalid { message, span }
            }
        },
        Err(ParseFail::Unsupported { message, span }) => {
            CreateTableOutcome::Unsupported { message, span }
        }
        Err(ParseFail::Invalid { message, span }) => CreateTableOutcome::Invalid { message, span },
    }
}

/// Same three-way contract as [`ParseOutcome`], for CREATE INDEX (spec
/// 002-parser, V3 block).
#[derive(Debug, Clone, PartialEq)]
pub enum CreateIndexOutcome {
    Accepted(Box<CreateIndex>),
    Unsupported { message: String, span: Span },
    Invalid { message: String, span: Span },
}

/// Parses a single CREATE INDEX statement (grammar
/// `.openspec/grammar/sqlite.ebnf` V3 block). Never panics — any input
/// produces one of the three [`CreateIndexOutcome`] variants.
pub fn parse_create_index(src: &str) -> CreateIndexOutcome {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_create_index_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => CreateIndexOutcome::Accepted(Box::new(stmt)),
            Err(ParseFail::Unsupported { message, span }) => {
                CreateIndexOutcome::Unsupported { message, span }
            }
            Err(ParseFail::Invalid { message, span }) => {
                CreateIndexOutcome::Invalid { message, span }
            }
        },
        Err(ParseFail::Unsupported { message, span }) => {
            CreateIndexOutcome::Unsupported { message, span }
        }
        Err(ParseFail::Invalid { message, span }) => CreateIndexOutcome::Invalid { message, span },
    }
}

/// Same three-way contract as [`ParseOutcome`], for DROP TABLE (spec
/// 002-parser, V3 block).
#[derive(Debug, Clone, PartialEq)]
pub enum DropTableOutcome {
    Accepted(Box<DropTable>),
    Unsupported { message: String, span: Span },
    Invalid { message: String, span: Span },
}

/// Parses a single DROP TABLE statement (grammar
/// `.openspec/grammar/sqlite.ebnf` V3 block). Never panics — any input
/// produces one of the three [`DropTableOutcome`] variants.
pub fn parse_drop_table(src: &str) -> DropTableOutcome {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_drop_table_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => DropTableOutcome::Accepted(Box::new(stmt)),
            Err(ParseFail::Unsupported { message, span }) => {
                DropTableOutcome::Unsupported { message, span }
            }
            Err(ParseFail::Invalid { message, span }) => {
                DropTableOutcome::Invalid { message, span }
            }
        },
        Err(ParseFail::Unsupported { message, span }) => {
            DropTableOutcome::Unsupported { message, span }
        }
        Err(ParseFail::Invalid { message, span }) => DropTableOutcome::Invalid { message, span },
    }
}

/// Same three-way contract as [`ParseOutcome`], for DROP INDEX (spec
/// 002-parser, V3 block).
#[derive(Debug, Clone, PartialEq)]
pub enum DropIndexOutcome {
    Accepted(Box<DropIndex>),
    Unsupported { message: String, span: Span },
    Invalid { message: String, span: Span },
}

/// Parses a single DROP INDEX statement (grammar
/// `.openspec/grammar/sqlite.ebnf` V3 block). Never panics — any input
/// produces one of the three [`DropIndexOutcome`] variants.
pub fn parse_drop_index(src: &str) -> DropIndexOutcome {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_drop_index_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => DropIndexOutcome::Accepted(Box::new(stmt)),
            Err(ParseFail::Unsupported { message, span }) => {
                DropIndexOutcome::Unsupported { message, span }
            }
            Err(ParseFail::Invalid { message, span }) => {
                DropIndexOutcome::Invalid { message, span }
            }
        },
        Err(ParseFail::Unsupported { message, span }) => {
            DropIndexOutcome::Unsupported { message, span }
        }
        Err(ParseFail::Invalid { message, span }) => DropIndexOutcome::Invalid { message, span },
    }
}
