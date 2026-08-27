// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Spike variant 002: `pomelo` — the Lemon parser generator as a Rust proc-macro.
//!
//! Grammar subset: `tests/spike/001_parser/grammar/sqlite-subset.ebnf`.
//! There is no build.rs / codegen step: the LALR(1) tables are generated at
//! compile time by the `pomelo!` macro in `grammar.rs`.

pub mod ast;
pub mod grammar;
pub mod tokenizer;

pub use ast::Stmt;

/// Parse a single SQL statement (no trailing `;`).
pub fn parse(sql: &str) -> Result<Stmt, String> {
    let tokens = tokenizer::tokenize(sql)?;
    let mut parser = grammar::Parser::new();
    for t in tokens {
        parser.parse(t.token).map_err(|e| {
            format!("{} (at offset {}, near {:?})", e, t.offset, t.text)
        })?;
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
