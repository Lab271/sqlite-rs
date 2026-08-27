// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! SQL tokenizer (spec 002-parser Requirement 1).
//!
//! Converts SQL source text into a stream of [`Token`]s, each carrying
//! a [`Span`] for error reporting. Malformed input never panics: it
//! produces a [`TokenKind::Error`] token and scanning continues.
//!
//! Kept independent of `src/schema/ddl_reader.rs` (Requirement 5) —
//! nothing here is imported by the minimal DDL reader.

/// Source location of a token: 1-based line/column of its first
/// character, plus the byte range `[offset, offset + len)` in the
/// original source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// 1-based line number of the token's first character.
    pub line: u32,
    /// 1-based column number of the token's first character.
    pub column: u32,
    /// Byte offset of the token's first character in the source.
    pub offset: u32,
    /// Length in bytes of the token's source text.
    pub len: u32,
}

/// A single lexed unit of SQL source: its [`TokenKind`] plus the [`Span`]
/// it occupies in the original text.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// The kind of token and any associated literal value.
    pub kind: TokenKind,
    /// Source location this token was scanned from.
    pub span: Span,
}

/// A `?NNN`/`:name`/`@name`/`$name` bind parameter, per spec 002-parser
/// Requirement 1's "Tokenize parameters" scenario (5 distinct kinds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Param {
    /// Bare `?`.
    Anonymous,
    /// `?NNN`.
    Numbered(u32),
    /// `:name`.
    Colon(String),
    /// `@name`.
    At(String),
    /// `$name`.
    Dollar(String),
}

/// The kind of a scanned [`Token`], carrying any literal value it holds.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    /// Integer literal.
    Integer(i64),
    /// Floating-point literal.
    Float(f64),
    /// String literal (quotes stripped, escapes resolved).
    String(String),
    /// `X'...'` blob literal, decoded to raw bytes.
    Blob(Vec<u8>),
    /// The `NULL` literal.
    Null,
    /// The `TRUE` literal.
    True,
    /// The `FALSE` literal.
    False,

    /// An unquoted or quoted identifier (table/column/etc. name).
    Identifier(String),
    /// A reserved SQL keyword.
    Keyword(Keyword),
    /// A bind parameter (`?`, `?NNN`, `:name`, `@name`, `$name`).
    Param(Param),

    // Punctuation / operators
    /// `*`
    Star,
    /// `,`
    Comma,
    /// `;`
    Semicolon,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `.`
    Dot,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `=` or `==`
    Eq,
    /// `!=` or `<>`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `||`
    Concat,
    /// `->`
    Arrow,
    /// `->>`
    ArrowArrow,
    /// `&`
    BitAnd,
    /// `|`
    BitOr,
    /// `~`
    BitNot,
    /// `<<`
    Shl,
    /// `>>`
    Shr,

    /// Malformed input; `String` is a human-readable reason.
    Error(String),
    /// End of input.
    Eof,
}

/// SQLite reserved words, excluding `NULL`/`TRUE`/`FALSE` which get
/// their own [`TokenKind`] literal variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum Keyword {
    /// The `ABORT` keyword.
    ABORT,
    /// The `ACTION` keyword.
    ACTION,
    /// The `ADD` keyword.
    ADD,
    /// The `AFTER` keyword.
    AFTER,
    /// The `ALL` keyword.
    ALL,
    /// The `ALTER` keyword.
    ALTER,
    /// The `ALWAYS` keyword.
    ALWAYS,
    /// The `ANALYZE` keyword.
    ANALYZE,
    /// The `AND` keyword.
    AND,
    /// The `AS` keyword.
    AS,
    /// The `ASC` keyword.
    ASC,
    /// The `ATTACH` keyword.
    ATTACH,
    /// The `AUTOINCREMENT` keyword.
    AUTOINCREMENT,
    /// The `BEFORE` keyword.
    BEFORE,
    /// The `BEGIN` keyword.
    BEGIN,
    /// The `BETWEEN` keyword.
    BETWEEN,
    /// The `BY` keyword.
    BY,
    /// The `CASCADE` keyword.
    CASCADE,
    /// The `CASE` keyword.
    CASE,
    /// The `CAST` keyword.
    CAST,
    /// The `CHECK` keyword.
    CHECK,
    /// The `COLLATE` keyword.
    COLLATE,
    /// The `COLUMN` keyword.
    COLUMN,
    /// The `COMMIT` keyword.
    COMMIT,
    /// The `CONFLICT` keyword.
    CONFLICT,
    /// The `CONSTRAINT` keyword.
    CONSTRAINT,
    /// The `CREATE` keyword.
    CREATE,
    /// The `CROSS` keyword.
    CROSS,
    /// The `CURRENT` keyword.
    CURRENT,
    /// The `CURRENT_DATE` keyword.
    CURRENT_DATE,
    /// The `CURRENT_TIME` keyword.
    CURRENT_TIME,
    /// The `CURRENT_TIMESTAMP` keyword.
    CURRENT_TIMESTAMP,
    /// The `DATABASE` keyword.
    DATABASE,
    /// The `DEFAULT` keyword.
    DEFAULT,
    /// The `DEFERRABLE` keyword.
    DEFERRABLE,
    /// The `DEFERRED` keyword.
    DEFERRED,
    /// The `DELETE` keyword.
    DELETE,
    /// The `DESC` keyword.
    DESC,
    /// The `DETACH` keyword.
    DETACH,
    /// The `DISTINCT` keyword.
    DISTINCT,
    /// The `DO` keyword.
    DO,
    /// The `DROP` keyword.
    DROP,
    /// The `EACH` keyword.
    EACH,
    /// The `ELSE` keyword.
    ELSE,
    /// The `END` keyword.
    END,
    /// The `ESCAPE` keyword.
    ESCAPE,
    /// The `EXCEPT` keyword.
    EXCEPT,
    /// The `EXCLUDE` keyword.
    EXCLUDE,
    /// The `EXCLUSIVE` keyword.
    EXCLUSIVE,
    /// The `EXISTS` keyword.
    EXISTS,
    /// The `EXPLAIN` keyword.
    EXPLAIN,
    /// The `FAIL` keyword.
    FAIL,
    /// The `FILTER` keyword.
    FILTER,
    /// The `FIRST` keyword.
    FIRST,
    /// The `FOLLOWING` keyword.
    FOLLOWING,
    /// The `FOR` keyword.
    FOR,
    /// The `FOREIGN` keyword.
    FOREIGN,
    /// The `FROM` keyword.
    FROM,
    /// The `FULL` keyword.
    FULL,
    /// The `GENERATED` keyword.
    GENERATED,
    /// The `GLOB` keyword.
    GLOB,
    /// The `GROUP` keyword.
    GROUP,
    /// The `GROUPS` keyword.
    GROUPS,
    /// The `HAVING` keyword.
    HAVING,
    /// The `IF` keyword.
    IF,
    /// The `IGNORE` keyword.
    IGNORE,
    /// The `IMMEDIATE` keyword.
    IMMEDIATE,
    /// The `IN` keyword.
    IN,
    /// The `INDEX` keyword.
    INDEX,
    /// The `INDEXED` keyword.
    INDEXED,
    /// The `INITIALLY` keyword.
    INITIALLY,
    /// The `INNER` keyword.
    INNER,
    /// The `INSERT` keyword.
    INSERT,
    /// The `INSTEAD` keyword.
    INSTEAD,
    /// The `INTERSECT` keyword.
    INTERSECT,
    /// The `INTO` keyword.
    INTO,
    /// The `IS` keyword.
    IS,
    /// The `ISNULL` keyword.
    ISNULL,
    /// The `JOIN` keyword.
    JOIN,
    /// The `KEY` keyword.
    KEY,
    /// The `LAST` keyword.
    LAST,
    /// The `LEFT` keyword.
    LEFT,
    /// The `LIKE` keyword.
    LIKE,
    /// The `LIMIT` keyword.
    LIMIT,
    /// The `MATCH` keyword.
    MATCH,
    /// The `MATERIALIZED` keyword.
    MATERIALIZED,
    /// The `NATURAL` keyword.
    NATURAL,
    /// The `NO` keyword.
    NO,
    /// The `NOT` keyword.
    NOT,
    /// The `NOTHING` keyword.
    NOTHING,
    /// The `NOTNULL` keyword.
    NOTNULL,
    /// The `NULLS` keyword.
    NULLS,
    /// The `OF` keyword.
    OF,
    /// The `OFFSET` keyword.
    OFFSET,
    /// The `ON` keyword.
    ON,
    /// The `OR` keyword.
    OR,
    /// The `ORDER` keyword.
    ORDER,
    /// The `OTHERS` keyword.
    OTHERS,
    /// The `OUTER` keyword.
    OUTER,
    /// The `OVER` keyword.
    OVER,
    /// The `PARTITION` keyword.
    PARTITION,
    /// The `PLAN` keyword.
    PLAN,
    /// The `PRAGMA` keyword.
    PRAGMA,
    /// The `PRECEDING` keyword.
    PRECEDING,
    /// The `PRIMARY` keyword.
    PRIMARY,
    /// The `QUERY` keyword.
    QUERY,
    /// The `RAISE` keyword.
    RAISE,
    /// The `RANGE` keyword.
    RANGE,
    /// The `RECURSIVE` keyword.
    RECURSIVE,
    /// The `REFERENCES` keyword.
    REFERENCES,
    /// The `REGEXP` keyword.
    REGEXP,
    /// The `REINDEX` keyword.
    REINDEX,
    /// The `RELEASE` keyword.
    RELEASE,
    /// The `RENAME` keyword.
    RENAME,
    /// The `REPLACE` keyword.
    REPLACE,
    /// The `RESTRICT` keyword.
    RESTRICT,
    /// The `RETURNING` keyword.
    RETURNING,
    /// The `RIGHT` keyword.
    RIGHT,
    /// The `ROLLBACK` keyword.
    ROLLBACK,
    /// The `ROW` keyword.
    ROW,
    /// The `ROWS` keyword.
    ROWS,
    /// The `SAVEPOINT` keyword.
    SAVEPOINT,
    /// The `SELECT` keyword.
    SELECT,
    /// The `SET` keyword.
    SET,
    /// The `TABLE` keyword.
    TABLE,
    /// The `TEMP` keyword.
    TEMP,
    /// The `TEMPORARY` keyword.
    TEMPORARY,
    /// The `THEN` keyword.
    THEN,
    /// The `TIES` keyword.
    TIES,
    /// The `TO` keyword.
    TO,
    /// The `TRANSACTION` keyword.
    TRANSACTION,
    /// The `TRIGGER` keyword.
    TRIGGER,
    /// The `UNBOUNDED` keyword.
    UNBOUNDED,
    /// The `UNION` keyword.
    UNION,
    /// The `UNIQUE` keyword.
    UNIQUE,
    /// The `UPDATE` keyword.
    UPDATE,
    /// The `USING` keyword.
    USING,
    /// The `VACUUM` keyword.
    VACUUM,
    /// The `VALUES` keyword.
    VALUES,
    /// The `VIEW` keyword.
    VIEW,
    /// The `VIRTUAL` keyword.
    VIRTUAL,
    /// The `WHEN` keyword.
    WHEN,
    /// The `WHERE` keyword.
    WHERE,
    /// The `WINDOW` keyword.
    WINDOW,
    /// The `WITH` keyword.
    WITH,
    /// The `WITHOUT` keyword.
    WITHOUT,
}

/// (uppercased keyword text, variant) sorted by text for binary search.
/// `NULL`/`TRUE`/`FALSE` are intentionally absent — see [`lookup_word`].
const KEYWORDS: &[(&str, Keyword)] = &[
    ("ABORT", Keyword::ABORT),
    ("ACTION", Keyword::ACTION),
    ("ADD", Keyword::ADD),
    ("AFTER", Keyword::AFTER),
    ("ALL", Keyword::ALL),
    ("ALTER", Keyword::ALTER),
    ("ALWAYS", Keyword::ALWAYS),
    ("ANALYZE", Keyword::ANALYZE),
    ("AND", Keyword::AND),
    ("AS", Keyword::AS),
    ("ASC", Keyword::ASC),
    ("ATTACH", Keyword::ATTACH),
    ("AUTOINCREMENT", Keyword::AUTOINCREMENT),
    ("BEFORE", Keyword::BEFORE),
    ("BEGIN", Keyword::BEGIN),
    ("BETWEEN", Keyword::BETWEEN),
    ("BY", Keyword::BY),
    ("CASCADE", Keyword::CASCADE),
    ("CASE", Keyword::CASE),
    ("CAST", Keyword::CAST),
    ("CHECK", Keyword::CHECK),
    ("COLLATE", Keyword::COLLATE),
    ("COLUMN", Keyword::COLUMN),
    ("COMMIT", Keyword::COMMIT),
    ("CONFLICT", Keyword::CONFLICT),
    ("CONSTRAINT", Keyword::CONSTRAINT),
    ("CREATE", Keyword::CREATE),
    ("CROSS", Keyword::CROSS),
    ("CURRENT", Keyword::CURRENT),
    ("CURRENT_DATE", Keyword::CURRENT_DATE),
    ("CURRENT_TIME", Keyword::CURRENT_TIME),
    ("CURRENT_TIMESTAMP", Keyword::CURRENT_TIMESTAMP),
    ("DATABASE", Keyword::DATABASE),
    ("DEFAULT", Keyword::DEFAULT),
    ("DEFERRABLE", Keyword::DEFERRABLE),
    ("DEFERRED", Keyword::DEFERRED),
    ("DELETE", Keyword::DELETE),
    ("DESC", Keyword::DESC),
    ("DETACH", Keyword::DETACH),
    ("DISTINCT", Keyword::DISTINCT),
    ("DO", Keyword::DO),
    ("DROP", Keyword::DROP),
    ("EACH", Keyword::EACH),
    ("ELSE", Keyword::ELSE),
    ("END", Keyword::END),
    ("ESCAPE", Keyword::ESCAPE),
    ("EXCEPT", Keyword::EXCEPT),
    ("EXCLUDE", Keyword::EXCLUDE),
    ("EXCLUSIVE", Keyword::EXCLUSIVE),
    ("EXISTS", Keyword::EXISTS),
    ("EXPLAIN", Keyword::EXPLAIN),
    ("FAIL", Keyword::FAIL),
    ("FILTER", Keyword::FILTER),
    ("FIRST", Keyword::FIRST),
    ("FOLLOWING", Keyword::FOLLOWING),
    ("FOR", Keyword::FOR),
    ("FOREIGN", Keyword::FOREIGN),
    ("FROM", Keyword::FROM),
    ("FULL", Keyword::FULL),
    ("GENERATED", Keyword::GENERATED),
    ("GLOB", Keyword::GLOB),
    ("GROUP", Keyword::GROUP),
    ("GROUPS", Keyword::GROUPS),
    ("HAVING", Keyword::HAVING),
    ("IF", Keyword::IF),
    ("IGNORE", Keyword::IGNORE),
    ("IMMEDIATE", Keyword::IMMEDIATE),
    ("IN", Keyword::IN),
    ("INDEX", Keyword::INDEX),
    ("INDEXED", Keyword::INDEXED),
    ("INITIALLY", Keyword::INITIALLY),
    ("INNER", Keyword::INNER),
    ("INSERT", Keyword::INSERT),
    ("INSTEAD", Keyword::INSTEAD),
    ("INTERSECT", Keyword::INTERSECT),
    ("INTO", Keyword::INTO),
    ("IS", Keyword::IS),
    ("ISNULL", Keyword::ISNULL),
    ("JOIN", Keyword::JOIN),
    ("KEY", Keyword::KEY),
    ("LAST", Keyword::LAST),
    ("LEFT", Keyword::LEFT),
    ("LIKE", Keyword::LIKE),
    ("LIMIT", Keyword::LIMIT),
    ("MATCH", Keyword::MATCH),
    ("MATERIALIZED", Keyword::MATERIALIZED),
    ("NATURAL", Keyword::NATURAL),
    ("NO", Keyword::NO),
    ("NOT", Keyword::NOT),
    ("NOTHING", Keyword::NOTHING),
    ("NOTNULL", Keyword::NOTNULL),
    ("NULLS", Keyword::NULLS),
    ("OF", Keyword::OF),
    ("OFFSET", Keyword::OFFSET),
    ("ON", Keyword::ON),
    ("OR", Keyword::OR),
    ("ORDER", Keyword::ORDER),
    ("OTHERS", Keyword::OTHERS),
    ("OUTER", Keyword::OUTER),
    ("OVER", Keyword::OVER),
    ("PARTITION", Keyword::PARTITION),
    ("PLAN", Keyword::PLAN),
    ("PRAGMA", Keyword::PRAGMA),
    ("PRECEDING", Keyword::PRECEDING),
    ("PRIMARY", Keyword::PRIMARY),
    ("QUERY", Keyword::QUERY),
    ("RAISE", Keyword::RAISE),
    ("RANGE", Keyword::RANGE),
    ("RECURSIVE", Keyword::RECURSIVE),
    ("REFERENCES", Keyword::REFERENCES),
    ("REGEXP", Keyword::REGEXP),
    ("REINDEX", Keyword::REINDEX),
    ("RELEASE", Keyword::RELEASE),
    ("RENAME", Keyword::RENAME),
    ("REPLACE", Keyword::REPLACE),
    ("RESTRICT", Keyword::RESTRICT),
    ("RETURNING", Keyword::RETURNING),
    ("RIGHT", Keyword::RIGHT),
    ("ROLLBACK", Keyword::ROLLBACK),
    ("ROW", Keyword::ROW),
    ("ROWS", Keyword::ROWS),
    ("SAVEPOINT", Keyword::SAVEPOINT),
    ("SELECT", Keyword::SELECT),
    ("SET", Keyword::SET),
    ("TABLE", Keyword::TABLE),
    ("TEMP", Keyword::TEMP),
    ("TEMPORARY", Keyword::TEMPORARY),
    ("THEN", Keyword::THEN),
    ("TIES", Keyword::TIES),
    ("TO", Keyword::TO),
    ("TRANSACTION", Keyword::TRANSACTION),
    ("TRIGGER", Keyword::TRIGGER),
    ("UNBOUNDED", Keyword::UNBOUNDED),
    ("UNION", Keyword::UNION),
    ("UNIQUE", Keyword::UNIQUE),
    ("UPDATE", Keyword::UPDATE),
    ("USING", Keyword::USING),
    ("VACUUM", Keyword::VACUUM),
    ("VALUES", Keyword::VALUES),
    ("VIEW", Keyword::VIEW),
    ("VIRTUAL", Keyword::VIRTUAL),
    ("WHEN", Keyword::WHEN),
    ("WHERE", Keyword::WHERE),
    ("WINDOW", Keyword::WINDOW),
    ("WITH", Keyword::WITH),
    ("WITHOUT", Keyword::WITHOUT),
];

/// Classifies an identifier-shaped word (already scanned) as a
/// keyword, `NULL`/`TRUE`/`FALSE` literal, or plain identifier.
fn lookup_word(word: &str) -> TokenKind {
    let upper = word.to_ascii_uppercase();
    match upper.as_str() {
        "NULL" => return TokenKind::Null,
        "TRUE" => return TokenKind::True,
        "FALSE" => return TokenKind::False,
        _ => {}
    }
    match KEYWORDS.binary_search_by(|(text, _)| (*text).cmp(upper.as_str())) {
        // `Ok(idx)` proves `idx` is in bounds, so `.get` never hits the
        // `unwrap_or_else` fallback; it's written this way (rather than
        // indexing) because the qualified subset denies
        // `clippy::indexing_slicing`/`unwrap_used`/`expect_used`.
        Ok(idx) => KEYWORDS
            .get(idx)
            .map(|(_, kw)| TokenKind::Keyword(*kw))
            .unwrap_or_else(|| TokenKind::Identifier(word.to_string())),
        Err(_) => TokenKind::Identifier(word.to_string()),
    }
}

/// Owns its input as a `Vec<(byte_offset, char)>` rather than borrowing
/// `&str` — the qualified subset (`make mvl-limit`) disallows explicit
/// lifetimes beyond function-scoped elision, so this can't hold a
/// borrowed `CharIndices` across calls.
pub struct Tokenizer {
    chars: Vec<(usize, char)>,
    pos: usize,
    src_len: usize,
    line: u32,
    column: u32,
}

impl Tokenizer {
    /// Creates a tokenizer positioned at the start of `src`.
    pub fn new(src: &str) -> Self {
        Tokenizer {
            chars: src.char_indices().collect(),
            pos: 0,
            src_len: src.len(),
            line: 1,
            column: 1,
        }
    }

    /// Tokenizes the whole input, including the trailing [`TokenKind::Eof`].
    pub fn tokenize(src: &str) -> Vec<Token> {
        let mut out = Vec::new();
        let mut tokenizer = Tokenizer::new(src);
        loop {
            let tok = tokenizer.next_token();
            let is_eof = matches!(tok.kind, TokenKind::Eof);
            out.push(tok);
            if is_eof {
                break;
            }
        }
        out
    }

    fn peek_char(&self) -> Option<char> {
        self.peek_at(0)
    }

    /// Looks `ahead` positions past the current position without
    /// consuming anything (`ahead == 0` is the current character).
    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars
            .get(self.pos.checked_add(ahead)?)
            .map(|&(_, c)| c)
    }

    fn peek_offset(&self) -> Option<usize> {
        self.chars.get(self.pos).map(|&(i, _)| i)
    }

    fn bump(&mut self) -> Option<char> {
        let &(_, c) = self.chars.get(self.pos)?;
        self.pos = self.pos.saturating_add(1);
        if c == '\n' {
            self.line = self.line.saturating_add(1);
            self.column = 1;
        } else {
            self.column = self.column.saturating_add(1);
        }
        Some(c)
    }

    fn current_pos(&self) -> (u32, u32, u32) {
        let offset = self.peek_offset().unwrap_or(self.src_len) as u32;
        (self.line, self.column, offset)
    }

    fn span_from(&self, start: (u32, u32, u32)) -> Span {
        let end_offset = self.peek_offset().unwrap_or(self.src_len) as u32;
        Span {
            line: start.0,
            column: start.1,
            offset: start.2,
            len: end_offset.saturating_sub(start.2),
        }
    }

    /// Skips whitespace and comments. Returns `Some(reason)` if an
    /// unterminated block comment ran to EOF.
    fn skip_trivia(&mut self) -> Option<String> {
        loop {
            match self.peek_char() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('-') => {
                    // Lookahead for `--` line comment without consuming
                    // a lone `-` (the Minus operator).
                    if self.peek_at(1) == Some('-') {
                        self.bump();
                        self.bump();
                        while let Some(c) = self.peek_char() {
                            if c == '\n' {
                                break;
                            }
                            self.bump();
                        }
                        continue;
                    }
                    break;
                }
                Some('/') => {
                    if self.peek_at(1) == Some('*') {
                        self.bump();
                        self.bump();
                        loop {
                            match self.peek_char() {
                                None => {
                                    return Some("unterminated block comment".to_string());
                                }
                                Some('*') => {
                                    self.bump();
                                    if self.peek_char() == Some('/') {
                                        self.bump();
                                        break;
                                    }
                                }
                                Some(_) => {
                                    self.bump();
                                }
                            }
                        }
                        continue;
                    }
                    break;
                }
                _ => break,
            }
        }
        None
    }

    /// Scans and returns the next [`Token`], including trivia skipping.
    /// Returns [`TokenKind::Eof`] once the input is exhausted; never panics.
    pub fn next_token(&mut self) -> Token {
        // Captured before `skip_trivia` so an unterminated-comment error
        // span points at the comment's start, not the EOF it scanned to.
        let trivia_start = self.current_pos();
        if let Some(reason) = self.skip_trivia() {
            return Token {
                kind: TokenKind::Error(reason),
                span: self.span_from(trivia_start),
            };
        }
        let start = self.current_pos();

        let Some(c) = self.peek_char() else {
            return Token {
                kind: TokenKind::Eof,
                span: self.span_from(start),
            };
        };

        let kind = match c {
            '0'..='9' => self.scan_number(),
            '.' => {
                // Lookahead: `.5` is a float; a lone `.` is Dot.
                if matches!(self.peek_at(1), Some('0'..='9')) {
                    self.scan_number()
                } else {
                    self.bump();
                    TokenKind::Dot
                }
            }
            '\'' => self.scan_string(),
            '"' => self.scan_quoted_identifier('"', '"'),
            '[' => self.scan_quoted_identifier('[', ']'),
            '`' => self.scan_quoted_identifier('`', '`'),
            '?' => self.scan_param_question(),
            ':' => self.scan_param_named(':'),
            '@' => self.scan_param_named('@'),
            '$' => self.scan_param_named('$'),
            c if c == 'x' || c == 'X' => self.scan_maybe_blob(c),
            c if is_ident_start(c) => self.scan_identifier_or_keyword(),
            _ => self.scan_operator(),
        };

        Token {
            kind,
            span: self.span_from(start),
        }
    }

    fn scan_identifier_or_keyword(&mut self) -> TokenKind {
        let mut word = String::new();
        while let Some(c) = self.peek_char() {
            if is_ident_continue(c) {
                word.push(c);
                self.bump();
            } else {
                break;
            }
        }
        lookup_word(&word)
    }

    /// `X'...'`/`x'...'` blob literal, or falls back to a plain
    /// identifier/keyword starting with `x`/`X`.
    fn scan_maybe_blob(&mut self, x: char) -> TokenKind {
        if self.peek_at(1) == Some('\'') {
            self.bump(); // consume x/X
            self.bump(); // consume opening '
            let mut hex = String::new();
            loop {
                match self.peek_char() {
                    None => {
                        return TokenKind::Error(format!(
                            "unterminated blob literal starting with {x}'"
                        ));
                    }
                    Some('\'') => {
                        self.bump();
                        break;
                    }
                    Some(c) => {
                        hex.push(c);
                        self.bump();
                    }
                }
            }
            if !hex.len().is_multiple_of(2) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return TokenKind::Error(format!("invalid blob literal hex digits: {hex:?}"));
            }
            let mut bytes = Vec::with_capacity(hex.len() / 2);
            for pair in hex.as_bytes().chunks(2) {
                let pair = std::str::from_utf8(pair).unwrap_or_default();
                match u8::from_str_radix(pair, 16) {
                    Ok(b) => bytes.push(b),
                    Err(_) => return TokenKind::Error(format!("invalid blob byte: {pair:?}")),
                }
            }
            TokenKind::Blob(bytes)
        } else {
            self.scan_identifier_or_keyword()
        }
    }

    fn scan_string(&mut self) -> TokenKind {
        self.bump(); // opening '
        let mut value = String::new();
        loop {
            match self.peek_char() {
                None => return TokenKind::Error("unterminated string literal".to_string()),
                Some('\'') => {
                    self.bump();
                    if self.peek_char() == Some('\'') {
                        value.push('\'');
                        self.bump();
                    } else {
                        break;
                    }
                }
                Some(c) => {
                    value.push(c);
                    self.bump();
                }
            }
        }
        TokenKind::String(value)
    }

    /// Quoted identifier with `open`/`close` delimiters. `"..."` and
    /// `` `...` `` double their closing delimiter to escape it (SQLite
    /// / MySQL convention); `[...]` has no escape mechanism.
    fn scan_quoted_identifier(&mut self, open: char, close: char) -> TokenKind {
        self.bump(); // opening delimiter
        let mut value = String::new();
        let escapes = open == close;
        loop {
            match self.peek_char() {
                None => {
                    return TokenKind::Error(format!(
                        "unterminated quoted identifier starting with {open:?}"
                    ));
                }
                Some(c) if c == close => {
                    self.bump();
                    if escapes && self.peek_char() == Some(close) {
                        value.push(close);
                        self.bump();
                    } else {
                        break;
                    }
                }
                Some(c) => {
                    value.push(c);
                    self.bump();
                }
            }
        }
        TokenKind::Identifier(value)
    }

    fn scan_param_question(&mut self) -> TokenKind {
        self.bump(); // '?'
        let mut digits = String::new();
        while let Some(c @ '0'..='9') = self.peek_char() {
            digits.push(c);
            self.bump();
        }
        if digits.is_empty() {
            TokenKind::Param(Param::Anonymous)
        } else {
            match digits.parse::<u32>() {
                Ok(n) => TokenKind::Param(Param::Numbered(n)),
                Err(_) => TokenKind::Error(format!("parameter number out of range: {digits}")),
            }
        }
    }

    fn scan_param_named(&mut self, sigil: char) -> TokenKind {
        self.bump(); // sigil
        let mut name = String::new();
        while let Some(c) = self.peek_char() {
            if is_ident_continue(c) {
                name.push(c);
                self.bump();
            } else {
                break;
            }
        }
        if name.is_empty() {
            return TokenKind::Error(format!("expected parameter name after {sigil:?}"));
        }
        match sigil {
            ':' => TokenKind::Param(Param::Colon(name)),
            '@' => TokenKind::Param(Param::At(name)),
            '$' => TokenKind::Param(Param::Dollar(name)),
            _ => TokenKind::Error(format!("unsupported parameter sigil {sigil:?}")),
        }
    }

    fn scan_number(&mut self) -> TokenKind {
        let mut text = String::new();
        let mut is_float = false;

        if self.peek_char() == Some('0') && matches!(self.peek_at(1), Some('x' | 'X')) {
            text.push(self.bump().unwrap_or_default());
            text.push(self.bump().unwrap_or_default());
            let mut hex = String::new();
            while let Some(c) = self.peek_char() {
                if c.is_ascii_hexdigit() {
                    hex.push(c);
                    self.bump();
                } else {
                    break;
                }
            }
            if hex.is_empty() {
                return TokenKind::Error("hex literal has no digits".to_string());
            }
            // SQLite parses hex integer literals as unsigned 64-bit and
            // bit-reinterprets them as signed i64 (values above i64::MAX
            // wrap to negative, e.g. 0xFFFFFFFFFFFFFFFF -> -1) — matched
            // here intentionally, not an unreviewed truncation.
            return match i64::from_str_radix(&hex, 16) {
                Ok(n) => TokenKind::Integer(n),
                Err(_) => match u64::from_str_radix(&hex, 16) {
                    Ok(n) => TokenKind::Integer(n as i64),
                    Err(e) => TokenKind::Error(format!("invalid hex literal: {e}")),
                },
            };
        }

        while let Some(c @ '0'..='9') = self.peek_char() {
            text.push(c);
            self.bump();
        }

        if self.peek_char() == Some('.') {
            is_float = true;
            text.push('.');
            self.bump();
            while let Some(c @ '0'..='9') = self.peek_char() {
                text.push(c);
                self.bump();
            }
        }

        if matches!(self.peek_char(), Some('e' | 'E')) {
            let sign_char = matches!(self.peek_at(1), Some('+' | '-'));
            let digits_ahead = if sign_char { 2 } else { 1 };
            let has_exp_digits = matches!(self.peek_at(digits_ahead), Some('0'..='9'));
            if has_exp_digits {
                is_float = true;
                text.push(self.bump().unwrap_or_default()); // e/E
                if sign_char {
                    text.push(self.bump().unwrap_or_default());
                }
                while let Some(c @ '0'..='9') = self.peek_char() {
                    text.push(c);
                    self.bump();
                }
            }
        }

        if is_float {
            match text.parse::<f64>() {
                Ok(f) => TokenKind::Float(f),
                Err(e) => TokenKind::Error(format!("invalid float literal {text:?}: {e}")),
            }
        } else {
            match text.parse::<i64>() {
                Ok(n) => TokenKind::Integer(n),
                Err(_) => match text.parse::<f64>() {
                    Ok(f) => TokenKind::Float(f),
                    Err(e) => TokenKind::Error(format!("invalid integer literal {text:?}: {e}")),
                },
            }
        }
    }

    fn scan_operator(&mut self) -> TokenKind {
        let c = match self.bump() {
            Some(c) => c,
            None => return TokenKind::Eof,
        };
        match c {
            '*' => TokenKind::Star,
            ',' => TokenKind::Comma,
            ';' => TokenKind::Semicolon,
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '+' => TokenKind::Plus,
            '/' => TokenKind::Slash,
            '%' => TokenKind::Percent,
            '~' => TokenKind::BitNot,
            '-' => {
                if self.peek_char() == Some('>') {
                    self.bump();
                    if self.peek_char() == Some('>') {
                        self.bump();
                        TokenKind::ArrowArrow
                    } else {
                        TokenKind::Arrow
                    }
                } else {
                    TokenKind::Minus
                }
            }
            '=' => {
                if self.peek_char() == Some('=') {
                    self.bump();
                }
                TokenKind::Eq
            }
            '!' => {
                if self.peek_char() == Some('=') {
                    self.bump();
                    TokenKind::Ne
                } else {
                    TokenKind::Error("expected '=' after '!'".to_string())
                }
            }
            '<' => match self.peek_char() {
                Some('=') => {
                    self.bump();
                    TokenKind::Le
                }
                Some('>') => {
                    self.bump();
                    TokenKind::Ne
                }
                Some('<') => {
                    self.bump();
                    TokenKind::Shl
                }
                _ => TokenKind::Lt,
            },
            '>' => match self.peek_char() {
                Some('=') => {
                    self.bump();
                    TokenKind::Ge
                }
                Some('>') => {
                    self.bump();
                    TokenKind::Shr
                }
                _ => TokenKind::Gt,
            },
            '|' => {
                if self.peek_char() == Some('|') {
                    self.bump();
                    TokenKind::Concat
                } else {
                    TokenKind::BitOr
                }
            }
            '&' => TokenKind::BitAnd,
            other => TokenKind::Error(format!("unexpected character {other:?}")),
        }
    }
}

/// Splits a multi-statement script into individual statement source
/// slices at top-level `;` boundaries — a `;` inside a string/blob
/// literal or a comment never splits, since this goes through the real
/// tokenizer rather than a naive `str::split(';')` (#358's CLI session
/// wiring: `sqlite-rs exec <db> "BEGIN; UPDATE ...; ROLLBACK;"` needs
/// each statement compiled and run separately, sharing one `Pager`).
/// Whether `sql` ends (ignoring trailing whitespace/comments) with a
/// top-level `;` — the REPL's (#365) "has the user finished typing a
/// statement, or do we need another line" test, going through the real
/// tokenizer so a `;` inside a string/blob literal spanning multiple
/// input lines never counts. Empty input is never complete.
pub fn ends_with_semicolon(sql: &str) -> bool {
    let tokens = Tokenizer::tokenize(sql);
    // `tokenize` always appends a trailing `Eof`; the token before it
    // (if any) is the last real token.
    let last_real = tokens.len().checked_sub(2).and_then(|i| tokens.get(i));
    matches!(last_real, Some(tok) if tok.kind == TokenKind::Semicolon)
}

/// Empty statements (a bare `;`, leading/trailing whitespace-only) are
/// dropped, matching `sqlite3`'s own script handling.
pub fn split_statements(sql: &str) -> Vec<String> {
    let tokens = Tokenizer::tokenize(sql);
    let mut statements = Vec::new();
    let mut start = 0usize;
    for tok in &tokens {
        match tok.kind {
            TokenKind::Semicolon => {
                let end = tok.span.offset as usize;
                push_trimmed(&mut statements, &sql[start..end]);
                start = (tok.span.offset as usize).saturating_add(tok.span.len as usize);
            }
            TokenKind::Eof => {
                push_trimmed(&mut statements, &sql[start..]);
            }
            _ => {}
        }
    }
    statements
}

fn push_trimmed(statements: &mut Vec<String>, slice: &str) {
    let trimmed = slice.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_string());
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;

    #[test]
    fn split_statements_splits_on_top_level_semicolons_and_trims_whitespace() {
        let stmts = split_statements("BEGIN;  UPDATE t SET a = 99 ;ROLLBACK");
        assert_eq!(stmts, vec!["BEGIN", "UPDATE t SET a = 99", "ROLLBACK"]);
    }

    #[test]
    fn split_statements_ignores_semicolons_inside_string_literals() {
        let stmts = split_statements("INSERT INTO t VALUES ('a;b'); SELECT 1");
        assert_eq!(
            stmts,
            vec![
                "INSERT INTO t VALUES ('a;b')".to_string(),
                "SELECT 1".to_string()
            ]
        );
    }

    #[test]
    fn split_statements_drops_empty_and_whitespace_only_statements() {
        let stmts = split_statements("  ; BEGIN ;  ; ROLLBACK ; ");
        assert_eq!(stmts, vec!["BEGIN", "ROLLBACK"]);
    }

    #[test]
    fn ends_with_semicolon_true_for_a_trailing_top_level_semicolon() {
        assert!(ends_with_semicolon("SELECT * FROM t;"));
        assert!(ends_with_semicolon("SELECT * FROM t ; -- trailing comment"));
    }

    #[test]
    fn ends_with_semicolon_false_when_no_trailing_semicolon() {
        assert!(!ends_with_semicolon("SELECT * FROM t"));
        assert!(!ends_with_semicolon(""));
        assert!(!ends_with_semicolon("   "));
    }

    #[test]
    fn ends_with_semicolon_ignores_a_semicolon_inside_a_string_literal() {
        assert!(!ends_with_semicolon("SELECT 'a;b'"));
        assert!(ends_with_semicolon("SELECT 'a;b';"));
    }

    fn kinds(src: &str) -> Vec<TokenKind> {
        Tokenizer::tokenize(src)
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    /// **Tests:** `src/parser/tokenizer.rs::test_tokenize_select`
    #[test]
    fn test_tokenize_select() {
        let got = kinds("SELECT a, b FROM t WHERE x > 10");
        assert_eq!(
            got,
            vec![
                TokenKind::Keyword(Keyword::SELECT),
                TokenKind::Identifier("a".to_string()),
                TokenKind::Comma,
                TokenKind::Identifier("b".to_string()),
                TokenKind::Keyword(Keyword::FROM),
                TokenKind::Identifier("t".to_string()),
                TokenKind::Keyword(Keyword::WHERE),
                TokenKind::Identifier("x".to_string()),
                TokenKind::Gt,
                TokenKind::Integer(10),
                TokenKind::Eof,
            ]
        );
    }

    /// **Tests:** `src/parser/tokenizer.rs::test_tokenize_string_literal_escaping`
    #[test]
    fn test_tokenize_string_literal_escaping() {
        let got = kinds("'hello''world'");
        assert_eq!(
            got,
            vec![TokenKind::String("hello'world".to_string()), TokenKind::Eof]
        );
    }

    /// **Tests:** `src/parser/tokenizer.rs::test_tokenize_blob_literal`
    #[test]
    fn test_tokenize_blob_literal() {
        let got = kinds("X'48454C4C4F'");
        assert_eq!(
            got,
            vec![TokenKind::Blob(vec![72, 69, 76, 76, 79]), TokenKind::Eof]
        );
    }

    /// **Tests:** `src/parser/tokenizer.rs::test_tokenize_parameters`
    #[test]
    fn test_tokenize_parameters() {
        let got = kinds("?, ?1, :name, @var, $param");
        assert_eq!(
            got,
            vec![
                TokenKind::Param(Param::Anonymous),
                TokenKind::Comma,
                TokenKind::Param(Param::Numbered(1)),
                TokenKind::Comma,
                TokenKind::Param(Param::Colon("name".to_string())),
                TokenKind::Comma,
                TokenKind::Param(Param::At("var".to_string())),
                TokenKind::Comma,
                TokenKind::Param(Param::Dollar("param".to_string())),
                TokenKind::Eof,
            ]
        );
    }

    /// `NULLS` (for `ORDER BY ... NULLS FIRST/LAST`) is a genuine
    /// SQLite keyword per `.openspec/grammar/sqlite.ebnf`'s sortlist
    /// rule, distinct from the `NULL` literal.
    #[test]
    fn test_nulls_keyword() {
        assert_eq!(
            kinds("NULLS FIRST"),
            vec![
                TokenKind::Keyword(Keyword::NULLS),
                TokenKind::Keyword(Keyword::FIRST),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_keyword_lookup_is_case_insensitive() {
        assert_eq!(kinds("select"), kinds("SELECT"));
        assert_eq!(kinds("SeLeCt"), kinds("SELECT"));
    }

    #[test]
    fn test_quoted_bracketed_backticked_identifiers() {
        assert_eq!(
            kinds(r#""a b""c""#),
            vec![TokenKind::Identifier("a b\"c".to_string()), TokenKind::Eof]
        );
        assert_eq!(
            kinds("[my col]"),
            vec![TokenKind::Identifier("my col".to_string()), TokenKind::Eof]
        );
        assert_eq!(
            kinds("`a``b`"),
            vec![TokenKind::Identifier("a`b".to_string()), TokenKind::Eof]
        );
    }

    #[test]
    fn test_null_true_false_are_literals_not_identifiers() {
        assert_eq!(
            kinds("NULL true FALSE"),
            vec![
                TokenKind::Null,
                TokenKind::True,
                TokenKind::False,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn test_float_forms() {
        assert_eq!(kinds("1e5"), vec![TokenKind::Float(1e5), TokenKind::Eof]);
        assert_eq!(kinds(".5"), vec![TokenKind::Float(0.5), TokenKind::Eof]);
        assert_eq!(kinds("1."), vec![TokenKind::Float(1.0), TokenKind::Eof]);
        assert_eq!(
            kinds("1.5e-3"),
            vec![TokenKind::Float(1.5e-3), TokenKind::Eof]
        );
    }

    #[test]
    fn test_hex_integer() {
        assert_eq!(kinds("0x1F"), vec![TokenKind::Integer(31), TokenKind::Eof]);
    }

    /// Hex literals above `i64::MAX` bit-wrap to negative rather than
    /// erroring, matching SQLite's unsigned-parse-then-reinterpret
    /// semantics — see the comment at the hex branch of `scan_number`.
    #[test]
    fn test_hex_integer_wraps_above_i64_max() {
        assert_eq!(
            kinds("0xFFFFFFFFFFFFFFFF"),
            vec![TokenKind::Integer(-1), TokenKind::Eof]
        );
    }

    /// `lookup_word`'s `KEYWORDS.binary_search_by` requires this table
    /// sorted by text; an out-of-order insertion would silently
    /// misclassify keywords instead of failing loudly.
    #[test]
    fn test_keywords_table_is_sorted() {
        assert!(KEYWORDS.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn test_operators_and_punctuation() {
        assert_eq!(
            kinds("|| -> ->> <= >= <> != == <<>>"),
            vec![
                TokenKind::Concat,
                TokenKind::Arrow,
                TokenKind::ArrowArrow,
                TokenKind::Le,
                TokenKind::Ge,
                TokenKind::Ne,
                TokenKind::Ne,
                TokenKind::Eq,
                TokenKind::Shl,
                TokenKind::Shr,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_comments_are_skipped() {
        assert_eq!(
            kinds("SELECT 1 -- trailing comment\nFROM t /* block\ncomment */ WHERE 1"),
            vec![
                TokenKind::Keyword(Keyword::SELECT),
                TokenKind::Integer(1),
                TokenKind::Keyword(Keyword::FROM),
                TokenKind::Identifier("t".to_string()),
                TokenKind::Keyword(Keyword::WHERE),
                TokenKind::Integer(1),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_malformed_input_never_panics() {
        let inputs = [
            "'unterminated",
            "X'ABC'",
            "X'ZZ'",
            "\"unterminated",
            "`unterminated",
            "/* unterminated",
            ":",
            "@",
            "$",
            "!",
            "\u{1}",
            "?4294967296000",
        ];
        for input in inputs {
            let toks = Tokenizer::tokenize(input);
            assert!(
                toks.iter().any(|t| matches!(t.kind, TokenKind::Error(_))),
                "expected an Error token for {input:?}, got {toks:?}"
            );
        }
    }

    #[test]
    fn test_spans_track_line_and_column() {
        let toks = Tokenizer::tokenize("SELECT 1\nFROM t");
        let select = &toks[0];
        assert_eq!(select.span.line, 1);
        assert_eq!(select.span.column, 1);
        assert_eq!(select.span.offset, 0);
        let from = &toks[2];
        assert_eq!(from.span.line, 2);
        assert_eq!(from.span.column, 1);
    }

    /// #368 tagged MC/DC vector (obligation `tokenizer_783`, match guard
    /// `c == 'x' || c == 'X'` dispatching to `scan_maybe_blob`): leaf A
    /// (`c == 'x'`) true.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__tokenizer_783__v1_lowercase_x() {
        assert_eq!(
            kinds("x'41'"),
            vec![TokenKind::Blob(vec![0x41]), TokenKind::Eof]
        );
    }

    /// #368 tagged MC/DC vector (obligation `tokenizer_783`): both
    /// leaves false — falls through to the identifier-start arm.
    /// Independence pair for A against
    /// `mcdc__tokenizer_783__v1_lowercase_x`.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__tokenizer_783__v2_neither_x_nor_capital_x() {
        assert_eq!(
            kinds("y"),
            vec![TokenKind::Identifier("y".to_string()), TokenKind::Eof]
        );
    }

    /// #368 tagged MC/DC vector (obligation `tokenizer_783`): leaf B
    /// (`c == 'X'`) true, leaf A false. Independence pair for B against
    /// `mcdc__tokenizer_783__v2_neither_x_nor_capital_x`.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__tokenizer_783__v3_uppercase_x() {
        assert_eq!(
            kinds("X'41'"),
            vec![TokenKind::Blob(vec![0x41]), TokenKind::Eof]
        );
    }

    /// #368 tagged MC/DC vector (obligation `tokenizer_831`, decision
    /// `!hex.len().is_multiple_of(2) || !hex.chars().all(is_ascii_hexdigit)`):
    /// leaf A (odd digit count) true.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__tokenizer_831__v1_odd_digit_count() {
        assert!(matches!(kinds("x'411'")[0], TokenKind::Error(_)));
    }

    /// #368 tagged MC/DC vector (obligation `tokenizer_831`): both
    /// leaves false — a valid, even-length, all-hex blob literal.
    /// Independence pair for A against
    /// `mcdc__tokenizer_831__v1_odd_digit_count`.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__tokenizer_831__v2_valid_hex() {
        assert_eq!(
            kinds("x'41'"),
            vec![TokenKind::Blob(vec![0x41]), TokenKind::Eof]
        );
    }

    /// #368 tagged MC/DC vector (obligation `tokenizer_831`): leaf B
    /// (a non-hex-digit character) true, leaf A false — even length, but
    /// not all hex digits. Independence pair for B against
    /// `mcdc__tokenizer_831__v2_valid_hex`.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__tokenizer_831__v3_even_length_non_hex_digit() {
        assert!(matches!(kinds("x'4g'")[0], TokenKind::Error(_)));
    }

    /// #368 tagged MC/DC vector (obligation `tokenizer_888`, decision
    /// `escapes && self.peek_char() == Some(close)` in
    /// `scan_quoted_identifier`): both leaves true — a doubled closing
    /// delimiter (`""`) inside a delimiter where open == close escapes.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__tokenizer_888__v1_escaped_doubled_delimiter() {
        assert_eq!(
            kinds(r#""a""b""#),
            vec![TokenKind::Identifier("a\"b".to_string()), TokenKind::Eof]
        );
    }

    /// #368 tagged MC/DC vector (obligation `tokenizer_888`): leaf A
    /// (`escapes`) false — `[...]` has no escape mechanism (open != close),
    /// so leaf B is never even reached. Independence pair for A against
    /// `mcdc__tokenizer_888__v1_escaped_doubled_delimiter`.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__tokenizer_888__v2_bracket_identifier_does_not_escape() {
        assert_eq!(
            kinds("[abc]"),
            vec![TokenKind::Identifier("abc".to_string()), TokenKind::Eof]
        );
    }

    /// #368 tagged MC/DC vector (obligation `tokenizer_888`): leaf A true,
    /// leaf B false — a simple double-quoted identifier with no doubled
    /// closing delimiter. Independence pair for B against
    /// `mcdc__tokenizer_888__v1_escaped_doubled_delimiter`.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__tokenizer_888__v3_unescaped_double_quoted() {
        assert_eq!(
            kinds("\"abc\""),
            vec![TokenKind::Identifier("abc".to_string()), TokenKind::Eof]
        );
    }

    /// #368 tagged MC/DC vector (obligation `tokenizer_947`, decision
    /// `self.peek_char() == Some('0') && matches!(self.peek_at(1), Some('x' | 'X'))`
    /// in `scan_number`): both leaves true — a hex literal.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__tokenizer_947__v1_hex_prefix() {
        assert_eq!(kinds("0x1A"), vec![TokenKind::Integer(26), TokenKind::Eof]);
    }

    /// #368 tagged MC/DC vector (obligation `tokenizer_947`): leaf A
    /// false — a number not starting with `0`. Independence pair for A
    /// against `mcdc__tokenizer_947__v1_hex_prefix`.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__tokenizer_947__v2_not_leading_zero() {
        assert_eq!(kinds("123"), vec![TokenKind::Integer(123), TokenKind::Eof]);
    }

    /// #368 tagged MC/DC vector (obligation `tokenizer_947`): leaf A
    /// true, leaf B false — a leading zero not followed by `x`/`X`,
    /// parsed as a plain decimal integer. Independence pair for B against
    /// `mcdc__tokenizer_947__v1_hex_prefix`.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__tokenizer_947__v3_leading_zero_not_hex() {
        assert_eq!(kinds("05"), vec![TokenKind::Integer(5), TokenKind::Eof]);
    }
}
