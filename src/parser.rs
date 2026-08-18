//! SQL parser (spec 002-parser). Lives independently of
//! `src/schema/ddl_reader.rs` (Requirement 5): the minimal DDL reader
//! must keep building and passing with this module absent, so nothing
//! under `src/schema` may depend on anything here.

pub mod ast;
pub mod error;
pub mod grammar;
pub mod printer;
pub mod tokenizer;

pub use error::{
    parse_delete, parse_insert, parse_select, DeleteOutcome, InsertOutcome, ParseOutcome,
};
