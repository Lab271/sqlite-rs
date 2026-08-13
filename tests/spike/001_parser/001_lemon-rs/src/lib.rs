//! sqlite-rs parser spike, variant 001: **lemon-rs**.
//!
//! The SQL subset from `tests/spike/001_parser/grammar/sqlite-subset.ebnf`
//! parsed with the same toolchain SQLite itself uses — the Lemon LALR(1)
//! parser generator — but emitting Rust via the lemon-rs `lempar.rs` driver
//! template. `build.rs` compiles `third_party/lemon/lemon.c` and runs it over
//! `src/parse.y` at build time.

pub mod ast;
pub mod parser;
pub mod tokenizer;

pub use ast::Stmt;
pub use parser::{parse, ParseError};
