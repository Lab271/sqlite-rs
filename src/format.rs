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
    let (mantissa, exp_str) = sci.split_once('e').expect("Rust {:e} always has an 'e'");
    let exp: i32 = exp_str
        .parse()
        .expect("Rust exponent is always a valid i32");
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
        let split = (exp as usize) + 1;
        let int_part = &digits[..split];
        let frac_part = trim_trailing_zeros(&digits[split..]);
        if frac_part.is_empty() {
            format!("{int_part}.0")
        } else {
            format!("{int_part}.{frac_part}")
        }
    } else {
        let leading_zeros = "0".repeat((-exp as usize) - 1);
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
    let mut s = String::with_capacity(3 + b.len() * 2);
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
/// for NULL, and `sqlite3`'s own quoting rule — not plain RFC4180. A
/// field is quoted (embedded `"` doubled) if it contains a comma,
/// double quote, CR, LF, **any embedded space**, or a **leading or
/// trailing single-quote character** (confirmed empirically against a
/// real `sqlite3` binary; not documented, and not standard RFC4180 —
/// see spike 003 finding 3).
pub fn format_csv_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format_real(*r),
        Value::Text(s) => csv_quote(s),
        Value::Blob(b) => csv_quote(&format_blob(b)),
    }
}

fn csv_quote(s: &str) -> String {
    // An empty string must still be quoted (`""`) — otherwise it's
    // indistinguishable from NULL, which prints as a true blank with no
    // quotes at all (confirmed against a real `sqlite3 -csv`).
    let needs_quote = s.is_empty()
        || s.contains([',', '"', '\n', '\r', ' '])
        || s.starts_with('\'')
        || s.ends_with('\'');
    if !needs_quote {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
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
        assert_eq!(csv_quote("mid'quote"), "mid'quote");
        assert_eq!(csv_quote(""), "\"\"");
        assert_eq!(format_csv_value(&Value::Null), "");
        assert_eq!(format_csv_value(&Value::Text(String::new())), "\"\"");
        assert_eq!(
            format_csv_value(&Value::Blob(vec![0xDE, 0xAD])),
            "\"X'DEAD'\""
        );
    }
}
