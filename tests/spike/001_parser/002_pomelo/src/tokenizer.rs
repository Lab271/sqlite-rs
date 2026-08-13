//! Hand-rolled tokenizer for the spike SQL subset.
//!
//! pomelo generates only the parser; the lexer is entirely ours. Tokens are the
//! `Token` enum that the `pomelo!` macro generated in `grammar.rs`.

use crate::grammar::Token;

/// A token plus enough source context to build a useful error message.
#[derive(Debug, Clone)]
pub struct Tok {
    pub token: Token,
    pub offset: usize,
    pub text: String,
}

fn keyword(word: &str) -> Option<Token> {
    // Keywords are unconditionally reserved here: this spike has no equivalent of
    // parse.y's `%fallback ID ...`, so `CREATE TABLE key (...)` would not parse.
    let t = match word.to_ascii_uppercase().as_str() {
        "ALL" => Token::All,
        "AND" => Token::And,
        "AS" => Token::As,
        "ASC" => Token::Asc,
        "BY" => Token::By,
        "CREATE" => Token::Create,
        "DELETE" => Token::Delete,
        "DESC" => Token::Desc,
        "DISTINCT" => Token::Distinct,
        "EXISTS" => Token::Exists,
        "FROM" => Token::From,
        "GROUP" => Token::Group,
        "HAVING" => Token::Having,
        "IF" => Token::If,
        "INSERT" => Token::Insert,
        "INTO" => Token::Into,
        "KEY" => Token::Key,
        "LIMIT" => Token::Limit,
        "NOT" => Token::Not,
        "NULL" => Token::Null,
        "OFFSET" => Token::Offset,
        "OR" => Token::Or,
        "ORDER" => Token::Order,
        "PRIMARY" => Token::Primary,
        "SELECT" => Token::Select,
        "SET" => Token::Set,
        "TABLE" => Token::Table,
        "UPDATE" => Token::Update,
        "VALUES" => Token::Values,
        "WHERE" => Token::Where,
        _ => return None,
    };
    Some(t)
}

pub fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();

    macro_rules! push {
        ($tok:expr, $start:expr, $len:expr) => {{
            out.push(Tok {
                token: $tok,
                offset: $start,
                text: src[$start..$start + $len].to_string(),
            });
            i = $start + $len;
        }};
    }

    while i < b.len() {
        let c = b[i];
        let start = i;

        // whitespace
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // -- line comment
        if c == b'-' && i + 1 < b.len() && b[i + 1] == b'-' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // identifier / keyword
        if c.is_ascii_alphabetic() || c == b'_' {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'$') {
                j += 1;
            }
            let word = &src[i..j];
            let tok = keyword(word).unwrap_or_else(|| Token::Id(word.to_string()));
            push!(tok, start, j - start);
            continue;
        }

        // "quoted identifier"
        if c == b'"' {
            let mut j = i + 1;
            loop {
                if j >= b.len() {
                    return Err(format!(
                        "tokenizer error at offset {}: unterminated quoted identifier",
                        start
                    ));
                }
                if b[j] == b'"' {
                    break;
                }
                j += 1;
            }
            let inner = src[i + 1..j].to_string();
            push!(Token::Id(inner), start, j + 1 - start);
            continue;
        }

        // 'string literal' with '' escape
        if c == b'\'' {
            let mut j = i + 1;
            let mut value = String::new();
            loop {
                if j >= b.len() {
                    return Err(format!(
                        "tokenizer error at offset {}: unterminated string literal",
                        start
                    ));
                }
                if b[j] == b'\'' {
                    if j + 1 < b.len() && b[j + 1] == b'\'' {
                        value.push('\'');
                        j += 2;
                        continue;
                    }
                    break;
                }
                value.push(b[j] as char);
                j += 1;
            }
            push!(Token::Str(value), start, j + 1 - start);
            continue;
        }

        // numeric literal
        if c.is_ascii_digit() {
            let mut j = i;
            let mut is_float = false;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j < b.len() && b[j] == b'.' {
                is_float = true;
                j += 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
            }
            if j < b.len() && (b[j] == b'e' || b[j] == b'E') {
                let mut k = j + 1;
                if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
                    k += 1;
                }
                if k < b.len() && b[k].is_ascii_digit() {
                    is_float = true;
                    j = k;
                    while j < b.len() && b[j].is_ascii_digit() {
                        j += 1;
                    }
                }
            }
            let text = &src[i..j];
            let tok = if is_float {
                Token::Float(text.parse::<f64>().map_err(|e| {
                    format!("tokenizer error at offset {}: bad float {:?}: {}", start, text, e)
                })?)
            } else {
                Token::Integer(text.parse::<i64>().map_err(|e| {
                    format!("tokenizer error at offset {}: bad integer {:?}: {}", start, text, e)
                })?)
            };
            push!(tok, start, j - start);
            continue;
        }

        // two-character operators
        if i + 1 < b.len() {
            let two = &src[i..i + 2];
            let tok = match two {
                "||" => Some(Token::Concat),
                "<>" | "!=" => Some(Token::Ne),
                "<=" => Some(Token::Le),
                ">=" => Some(Token::Ge),
                "==" => Some(Token::Eq),
                _ => None,
            };
            if let Some(tok) = tok {
                push!(tok, start, 2);
                continue;
            }
        }

        // single-character tokens
        let tok = match c {
            b'(' => Token::LParen,
            b')' => Token::RParen,
            b',' => Token::Comma,
            b'.' => Token::Dot,
            b'=' => Token::Eq,
            b'<' => Token::Lt,
            b'>' => Token::Gt,
            b'+' => Token::Plus,
            b'-' => Token::Minus,
            b'*' => Token::Star,
            b'/' => Token::Slash,
            b'%' => Token::Rem,
            _ => {
                return Err(format!(
                    "tokenizer error at offset {}: unexpected character {:?}",
                    start, c as char
                ));
            }
        };
        push!(tok, start, 1);
    }

    Ok(out)
}
