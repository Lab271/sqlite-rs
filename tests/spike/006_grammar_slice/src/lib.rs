// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Spike 006: does slicing the grammar down to V2 SELECT-core work?
//!
//! See `.openspec/grammar/sqlite.ebnf` for the V-block-tagged grammar source
//! of truth, and `FINDINGS.md` in this directory for the writeup.

pub mod ast;
pub mod grammar;
pub mod tokenizer;
pub mod unsupported;

pub use ast::Select;

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Accepted(Select),
    Unsupported(&'static str),
    SyntaxError(String),
}

/// Parse a single SQL statement (no trailing `;`), classifying the result
/// into the three-way outcome the spike's falsification criterion requires.
pub fn parse(sql: &str) -> Outcome {
    match parse_strict(sql) {
        Ok(select) => Outcome::Accepted(select),
        Err(e) => match unsupported::classify_unsupported(sql) {
            Some(feature) => Outcome::Unsupported(feature),
            None => Outcome::SyntaxError(e),
        },
    }
}

fn parse_strict(sql: &str) -> Result<Select, String> {
    let tokens = tokenizer::tokenize(sql)?;
    let mut parser = grammar::Parser::new();
    for t in tokens {
        parser
            .parse(t.token)
            .map_err(|e| format!("{} (at offset {}, near {:?})", e, t.offset, t.text))?;
    }
    parser.end_of_input()
}

/// Split a fixture file into statements on `;` + newline.
pub fn split_statements(content: &str) -> Vec<&str> {
    content
        .split(";\n")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect()
}
