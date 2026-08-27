// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Property-based tokenize/print roundtrip test for `src/parser/tokenizer.rs`.
//!
//! Lives outside `src/` for the same reason as `record_proptest.rs`:
//! `proptest!`'s macro expansion isn't in the qualified subset's
//! curated macro allowlist (issue #23 / `make mvl-limit`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use proptest::prelude::*;
use sqlite_rs::parser::tokenizer::{TokenKind, Tokenizer};

/// A small alphabet of tokens with an unambiguous, round-trippable
/// source rendering, used to build random token streams and confirm
/// `print(tokenize(x)) tokenizes back to x` — i.e. the tokenizer is a
/// faithful (if lossy on whitespace/case) inverse of rendering.
#[derive(Debug, Clone)]
enum SampleToken {
    Select,
    From,
    Where,
    Ident(String),
    Integer(i64),
    Comma,
    Star,
    Eq,
    Lt,
    Plus,
}

impl SampleToken {
    fn render(&self) -> String {
        match self {
            SampleToken::Select => "SELECT".to_string(),
            SampleToken::From => "FROM".to_string(),
            SampleToken::Where => "WHERE".to_string(),
            SampleToken::Ident(s) => s.clone(),
            SampleToken::Integer(n) => n.to_string(),
            SampleToken::Comma => ",".to_string(),
            SampleToken::Star => "*".to_string(),
            SampleToken::Eq => "=".to_string(),
            SampleToken::Lt => "<".to_string(),
            SampleToken::Plus => "+".to_string(),
        }
    }

    fn expected_kind(&self) -> TokenKind {
        use sqlite_rs::parser::tokenizer::Keyword;
        match self {
            SampleToken::Select => TokenKind::Keyword(Keyword::SELECT),
            SampleToken::From => TokenKind::Keyword(Keyword::FROM),
            SampleToken::Where => TokenKind::Keyword(Keyword::WHERE),
            SampleToken::Ident(s) => TokenKind::Identifier(s.clone()),
            SampleToken::Integer(n) => TokenKind::Integer(*n),
            SampleToken::Comma => TokenKind::Comma,
            SampleToken::Star => TokenKind::Star,
            SampleToken::Eq => TokenKind::Eq,
            SampleToken::Lt => TokenKind::Lt,
            SampleToken::Plus => TokenKind::Plus,
        }
    }
}

fn sample_token_strategy() -> impl Strategy<Value = SampleToken> {
    prop_oneof![
        Just(SampleToken::Select),
        Just(SampleToken::From),
        Just(SampleToken::Where),
        // Filtered to avoid colliding with a reserved keyword or
        // NULL/TRUE/FALSE, which the tokenizer classifies specially
        // rather than as a plain identifier.
        "[a-z][a-z0-9_]{0,8}"
            .prop_filter("must not be a keyword", |s| {
                !matches!(
                    sqlite_rs::parser::tokenizer::Tokenizer::tokenize(s)
                        .into_iter()
                        .next()
                        .map(|t| t.kind),
                    Some(TokenKind::Keyword(_))
                        | Some(TokenKind::Null)
                        | Some(TokenKind::True)
                        | Some(TokenKind::False)
                )
            })
            .prop_map(SampleToken::Ident),
        // Non-negative only: a literal `-1` tokenizes as `Minus, Integer(1)`
        // (unary minus is the parser's job, not the tokenizer's).
        any::<u32>().prop_map(|n| SampleToken::Integer(n as i64)),
        Just(SampleToken::Comma),
        Just(SampleToken::Star),
        Just(SampleToken::Eq),
        Just(SampleToken::Lt),
        Just(SampleToken::Plus),
    ]
}

proptest! {
    /// Rendering a random token stream with single-space separators and
    /// re-tokenizing it recovers exactly the same sequence of kinds.
    #[test]
    fn tokenize_print_roundtrip(tokens in prop::collection::vec(sample_token_strategy(), 0..16)) {
        let rendered = tokens
            .iter()
            .map(SampleToken::render)
            .collect::<Vec<_>>()
            .join(" ");

        let got: Vec<TokenKind> = Tokenizer::tokenize(&rendered)
            .into_iter()
            .map(|t| t.kind)
            .filter(|k| !matches!(k, TokenKind::Eof))
            .collect();

        let expected: Vec<TokenKind> = tokens.iter().map(SampleToken::expected_kind).collect();

        prop_assert_eq!(got, expected);
    }

    /// The tokenizer never panics on arbitrary byte input (interpreted
    /// as UTF-8 lossily), only ever emitting tokens up to and including
    /// an `Eof`.
    #[test]
    fn tokenize_never_panics_on_arbitrary_text(src in ".{0,64}") {
        let toks = Tokenizer::tokenize(&src);
        prop_assert!(matches!(toks.last().map(|t| &t.kind), Some(TokenKind::Eof)));
    }
}
