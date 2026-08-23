//! The three-way SELECT-core parse outcome (spec 002-parser
//! Requirement 4), per spike 006 (#57)'s verdict: `sqlite3` only ever
//! accepts or rejects, but sqlite-rs's V2 slice additionally needs to
//! distinguish "syntactically valid SQL we haven't implemented yet"
//! (e.g. `JOIN`) from "actually malformed" — otherwise a not-yet-built
//! feature reads identically to a typo.

use super::ast::{
    Begin, Commit, CreateIndex, CreateTable, CreateView, Delete, DropIndex, DropTable, DropView,
    Explain, Insert, Rollback, Select, Update,
};
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

/// Parses `EXPLAIN [QUERY PLAN] select-stmt` (#243, grammar V4). Never
/// panics — any input produces one of the three [`ParseOutcome`]
/// variants. Bare `EXPLAIN` (no `QUERY PLAN`) and non-`SELECT` bodies
/// are `Unsupported`, not `Invalid` — syntactically recognized SQL this
/// entry point doesn't implement, per [`ParseOutcome`]'s three-way
/// contract.
pub fn parse_explain(src: &str) -> ParseOutcome<Explain> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_explain_stmt() {
        Ok(explain) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(explain)),
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

/// Parses a single INSERT statement (grammar `.openspec/grammar/sqlite.ebnf`
/// V3 block). Never panics — any input produces one of the three
/// [`ParseOutcome`] variants.
pub fn parse_insert(src: &str) -> ParseOutcome<Insert> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_insert_stmt() {
        Ok(insert) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(insert)),
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

/// Parses a single DELETE statement (grammar `.openspec/grammar/sqlite.ebnf`
/// V3 block). Never panics — any input produces one of the three
/// [`ParseOutcome`] variants.
pub fn parse_delete(src: &str) -> ParseOutcome<Delete> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_delete_stmt() {
        Ok(delete) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(delete)),
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

/// Parses a single CREATE TABLE statement (grammar
/// `.openspec/grammar/sqlite.ebnf` V3 block). Never panics — any input
/// produces one of the three [`ParseOutcome`] variants.
pub fn parse_create_table(src: &str) -> ParseOutcome<CreateTable> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_create_table_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(stmt)),
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

/// Parses a single CREATE INDEX statement (grammar
/// `.openspec/grammar/sqlite.ebnf` V3 block). Never panics — any input
/// produces one of the three [`ParseOutcome`] variants.
pub fn parse_create_index(src: &str) -> ParseOutcome<CreateIndex> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_create_index_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(stmt)),
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

/// Parses a single CREATE VIEW statement (grammar
/// `.openspec/grammar/sqlite.ebnf` V6 block, #379). Never panics — any
/// input produces one of the three [`ParseOutcome`] variants.
pub fn parse_create_view(src: &str) -> ParseOutcome<CreateView> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_create_view_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(stmt)),
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

/// Parses a single DROP VIEW statement (grammar
/// `.openspec/grammar/sqlite.ebnf` V6 block, #379). Never panics — any
/// input produces one of the three [`ParseOutcome`] variants.
pub fn parse_drop_view(src: &str) -> ParseOutcome<DropView> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_drop_view_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(stmt)),
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

/// Parses a single DROP TABLE statement (grammar
/// `.openspec/grammar/sqlite.ebnf` V3 block). Never panics — any input
/// produces one of the three [`ParseOutcome`] variants.
pub fn parse_drop_table(src: &str) -> ParseOutcome<DropTable> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_drop_table_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(stmt)),
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

/// Parses a single DROP INDEX statement (grammar
/// `.openspec/grammar/sqlite.ebnf` V3 block). Never panics — any input
/// produces one of the three [`ParseOutcome`] variants.
pub fn parse_drop_index(src: &str) -> ParseOutcome<DropIndex> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_drop_index_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(stmt)),
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

/// Parses a single BEGIN statement (grammar `.openspec/grammar/sqlite.ebnf`
/// V5 block, #356). Never panics — any input produces one of the three
/// [`ParseOutcome`] variants.
pub fn parse_begin(src: &str) -> ParseOutcome<Begin> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_begin_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(stmt)),
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

/// Parses a single COMMIT/END statement (grammar
/// `.openspec/grammar/sqlite.ebnf` V5 block, #356). Never panics — any input
/// produces one of the three [`ParseOutcome`] variants.
pub fn parse_commit(src: &str) -> ParseOutcome<Commit> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_commit_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(stmt)),
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

/// Parses a single ROLLBACK statement (grammar
/// `.openspec/grammar/sqlite.ebnf` V5 block, #356). Never panics — any input
/// produces one of the three [`ParseOutcome`] variants.
pub fn parse_rollback(src: &str) -> ParseOutcome<Rollback> {
    let tokens = Tokenizer::tokenize(src);
    let mut parser = Parser::new(tokens);
    match parser.parse_rollback_stmt() {
        Ok(stmt) => match parser.expect_end() {
            Ok(()) => ParseOutcome::Accepted(Box::new(stmt)),
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
