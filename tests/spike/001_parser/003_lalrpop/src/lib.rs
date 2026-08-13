//! Parser-toolchain spike variant 003: LALRPOP (LR(1) generator, `.lalrpop` DSL,
//! code-generated at build time by `build.rs`).

pub mod ast;

lalrpop_util::lalrpop_mod!(
    #[allow(clippy::all)]
    pub grammar
);

/// `StatementParser::new()` compiles the built-in lexer's regex set, so it is
/// expensive (~1ms). Build it once and reuse it -- this is the idiomatic
/// LALRPOP usage and the only fair thing to measure.
fn parser() -> &'static grammar::StatementParser {
    static P: std::sync::OnceLock<grammar::StatementParser> = std::sync::OnceLock::new();
    P.get_or_init(grammar::StatementParser::new)
}

/// Parse a single SQL statement (trailing `;` optional).
pub fn parse(sql: &str) -> Result<ast::Stmt, String> {
    parser().parse(sql).map_err(|e| e.to_string())
}

/// Split a fixture file into individual statements on `;` + newline.
/// The `;` is kept on each statement (the grammar accepts it optionally).
pub fn split_statements(src: &str) -> Vec<String> {
    src.split(";\n")
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with("--"))
        .map(|s| if s.ends_with(';') { s.to_string() } else { format!("{s};") })
        .collect()
}
