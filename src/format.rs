//! SQLite shell-parity value rendering, shared by `dump` (`-list` mode)
//! and `export` (`-csv` mode). See `.openspec/specs/003-file-format`'s
//! REAL round-trip note and spike 002/003's findings on float display
//! and CSV quoting — this module is where those open questions get
//! resolved into concrete formatting rules.

use crate::record::Value;

/// Renders a REAL the way `sqlite3`'s `-list`/`-csv` modes do: 15
/// significant digits (`%.15g`-equivalent), switching to scientific
/// notation when the decimal exponent is `< -4` or `>= 15`, and always
/// keeping an explicit decimal point or exponent — SQLite's own rule
/// that a REAL never prints as a bare integer, so `1.0` never becomes
/// `1`.
///
/// Note: `sqlite3 .dump`'s `quote()`-based REAL rendering uses a
/// different, higher-precision routine (observed ~19 significant
/// digits) that this module does not replicate — out of scope per
/// issue #37, whose output-contract requirement is `-csv`/`-list`
/// parity, not `.dump`'s SQL-literal precision.
pub fn format_real(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "0.0".to_string()
        };
    }
    // Not reachable from valid on-disk storage: SQLite's REAL serial
    // types decode to a finite f64, never NaN. Guarded defensively rather
    // than left to fall through into the exponent math below, which
    // assumes a finite, non-zero value.
    if x.is_nan() {
        return "NULL".to_string();
    }

    let neg = x.is_sign_negative();
    let ax = x.abs();
    if ax.is_infinite() {
        let mag = "9.0e+999"; // sqlite has no literal infinity display; unreachable via decoded storage
        return if neg {
            format!("-{mag}")
        } else {
            mag.to_string()
        };
    }

    let sci = format!("{:.14e}", ax);
    // Rust's `{:.14e}` formatter always emits an `e<exponent>` suffix with
    // a valid integer exponent — these fallbacks are unreachable in
    // practice, not real error handling.
    let (mantissa, exp_str) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp_str.parse().unwrap_or(0);
    // Rust's `{:.14e}` always yields exactly 1 + 14 = 15 significant digits.
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();

    let body = if !(-4..15).contains(&exp) {
        let mantissa_trimmed = trim_trailing_zeros(&digits[1..]);
        let mantissa_part = if mantissa_trimmed.is_empty() {
            format!("{}.0", &digits[..1])
        } else {
            format!("{}.{}", &digits[..1], mantissa_trimmed)
        };
        let exp_sign = if exp >= 0 { "+" } else { "-" };
        format!("{mantissa_part}e{exp_sign}{:02}", exp.abs())
    } else if exp >= 0 {
        let split = (exp as usize).saturating_add(1);
        let int_part = &digits[..split];
        let frac_part = trim_trailing_zeros(&digits[split..]);
        if frac_part.is_empty() {
            format!("{int_part}.0")
        } else {
            format!("{int_part}.{frac_part}")
        }
    } else {
        let leading_zeros = "0".repeat((exp.unsigned_abs() as usize).saturating_sub(1));
        let frac = trim_trailing_zeros(&digits);
        format!("0.{leading_zeros}{frac}")
    };

    if neg {
        format!("-{body}")
    } else {
        body
    }
}

fn trim_trailing_zeros(s: &str) -> &str {
    s.trim_end_matches('0')
}

/// Renders a blob the way `sqlite3`'s `quote()` does: `X'` + uppercase
/// hex + `'`. Neither `-list` nor `-csv` mode has any other way to print
/// raw, possibly non-UTF8 blob bytes safely.
pub fn format_blob(b: &[u8]) -> String {
    let mut s = String::with_capacity(3usize.saturating_add(b.len().saturating_mul(2)));
    s.push_str("X'");
    for byte in b {
        s.push_str(&format!("{byte:02X}"));
    }
    s.push('\'');
    s
}

/// Renders a value for `-list` mode (what `dump` prints): `NULL`
/// literal for nulls, raw unescaped text (list mode does no escaping at
/// all — the separator's own ambiguity if it appears inside a value is
/// inherited from `sqlite3`, not introduced here), and `X'HEX'` blobs.
pub fn format_list_value(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format_real(*r),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => format_blob(b),
    }
}

/// Renders a value for `-csv` mode (what `export` prints): empty string
/// for NULL, and `sqlite3`'s own quoting rule — not plain RFC4180. See
/// [`csv_char_forces_quote`] for the exact rule.
///
/// Spike 003 finding 3 described this rule as "any embedded space, or a
/// leading or trailing single-quote"; that was incomplete and wrong in
/// both directions (a single-quote *anywhere* quotes, as do tabs, other
/// control characters, DEL, and all non-ASCII). The rule here is derived
/// from a systematic byte-by-byte probe of the pinned oracle instead —
/// see `tests/corpus/cli_e2e_test.rs`'s
/// `csv_quote_matches_oracle_on_edge_values`, which pins it (#55).
pub fn format_csv_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format_real(*r),
        Value::Text(s) => csv_quote(s),
        Value::Blob(b) => csv_quote(&format_blob(b)),
    }
}

/// Whether `c` on its own forces the whole value to be quoted, per
/// `sqlite3`'s `needCsvQuote` byte table (`shell.c`) plus its separate
/// check for the column separator.
///
/// Established by probing the pinned oracle across every byte 0x01–0x7F
/// and representative multi-byte characters: the bytes that come back
/// *unquoted* are exactly `0x21..=0x7E` minus `"`, `'`, and `,`.
/// Everything else quotes — control characters (tab included), space,
/// DEL, and every non-ASCII character, since `needCsvQuote` marks all
/// bytes `>= 0x80` and a non-ASCII `char`'s UTF-8 encoding is entirely
/// made of such bytes.
///
/// The separator is hardcoded to `,` here because that is the only
/// separator this crate emits; `sqlite3` compares against its
/// configurable `colSeparator` instead.
fn csv_char_forces_quote(c: char) -> bool {
    !matches!(c, '\u{21}'..='\u{7E}') || c == '"' || c == '\'' || c == ','
}

/// Applies `sqlite3`'s CSV quoting heuristic to an arbitrary string —
/// exposed (not just used internally by [`format_csv_value`]) because
/// CSV column headers need the same quoting: a table's declared column
/// name can itself contain a comma, quote, space, or single-quote.
///
/// The heuristic is not RFC 4180 — see [`csv_char_forces_quote`] for the
/// exact rule and how it was established. An empty string is also quoted
/// (`""`), otherwise it would be indistinguishable from NULL, which prints
/// as a true blank with no quotes at all.
///
/// This only ever *adds* quoting and doubles embedded `"`. It never
/// rewrites the value's own bytes — an embedded CR or LF is quoted and
/// passed through verbatim, since SQLite stores TEXT byte-for-byte and a
/// reader must not invent line-ending translation the storage engine
/// doesn't do.
pub fn csv_quote(s: &str) -> String {
    let needs_quote = s.is_empty() || s.chars().any(csv_char_forces_quote);
    if !needs_quote {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('"');
    for c in s.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_matches_oracle_thresholds() {
        #[allow(clippy::approx_constant)]
        let three_point_one_four = 3.14;
        assert_eq!(format_real(three_point_one_four), "3.14");
        assert_eq!(format_real(1.0), "1.0");
        assert_eq!(format_real(2.5e300), "2.5e+300");
        assert_eq!(format_real(0.0001), "0.0001");
        assert_eq!(format_real(100000000000000.0), "100000000000000.0");
        assert_eq!(format_real(1e15), "1.0e+15");
        assert_eq!(format_real(999999999999999.0), "999999999999999.0");
        assert_eq!(format_real(0.00001), "1.0e-05");
        assert_eq!(format_real(-2.5), "-2.5");
        assert_eq!(format_real(123.456), "123.456");
        assert_eq!(format_real(0.0), "0.0");
        assert_eq!(format_real(-0.0), "-0.0");
    }

    #[test]
    fn blob_renders_as_quote_style_hex() {
        assert_eq!(format_blob(&[0xDE, 0xAD, 0xBE, 0xEF]), "X'DEADBEEF'");
        assert_eq!(format_blob(&[]), "X''");
    }

    #[test]
    fn csv_quoting_matches_sqlite_heuristic() {
        assert_eq!(csv_quote("ab"), "ab");
        assert_eq!(csv_quote("a b"), "\"a b\"");
        assert_eq!(csv_quote(" ab"), "\" ab\"");
        assert_eq!(csv_quote("ab "), "\"ab \"");
        assert_eq!(csv_quote("ends_with_quote'"), "\"ends_with_quote'\"");
        assert_eq!(csv_quote("'starts"), "\"'starts\"");
        // Any single quote anywhere forces quoting, not just a leading or
        // trailing one. Spike 003's finding 3 concluded the opposite and
        // this test previously pinned that conclusion; both were wrong.
        // Verified against the pinned oracle (3.53.3) — and against
        // Apple's 3.51 build, which agrees, so the earlier error was a
        // mis-probe rather than a version difference.
        assert_eq!(csv_quote("mid'quote"), "\"mid'quote\"");
        assert_eq!(csv_quote("a'b"), "\"a'b\"");

        // Control characters — tab included — force quoting. Previously
        // unquoted, caught by the CLI oracle diff (#55).
        //
        // Quoting is all this function does: it never rewrites the value's
        // bytes. Note the `sqlite3` *shell* additionally escapes most
        // control characters into caret notation (`\u{7}` → `^G`) as
        // terminal safety, which this crate does not reproduce — that is a
        // separate, open decision, not part of the CSV quoting rule.
        assert_eq!(csv_quote("tab\tsep"), "\"tab\tsep\"");
        assert_eq!(csv_quote("bell\u{7}"), "\"bell\u{7}\"");
        assert_eq!(csv_quote("del\u{7f}"), "\"del\u{7f}\"");

        // Embedded CR/LF are quoted and passed through byte-for-byte —
        // never translated. SQLite stores TEXT verbatim, so a reader must
        // not invent line-ending conversion.
        assert_eq!(csv_quote("a\r\nb"), "\"a\r\nb\"");
        assert_eq!(csv_quote("a\nb"), "\"a\nb\"");

        // Every non-ASCII character forces quoting: `needCsvQuote` marks
        // all bytes >= 0x80, and a non-ASCII char is made entirely of
        // those. Also previously unquoted.
        assert_eq!(csv_quote("café"), "\"café\"");
        assert_eq!(csv_quote("日本"), "\"日本\"");
        assert_eq!(csv_quote("nbsp\u{a0}"), "\"nbsp\u{a0}\"");

        // The boundary of the bare range: 0x21..=0x7E minus " ' ,
        assert_eq!(
            csv_quote("!#$%&()*+-./:;<=>?@[\\]^_`{|}~"),
            "!#$%&()*+-./:;<=>?@[\\]^_`{|}~"
        );
        assert_eq!(csv_quote(""), "\"\"");
        assert_eq!(format_csv_value(&Value::Null), "");
        assert_eq!(format_csv_value(&Value::Text(String::new())), "\"\"");
        assert_eq!(
            format_csv_value(&Value::Blob(vec![0xDE, 0xAD])),
            "\"X'DEAD'\""
        );
    }
}
