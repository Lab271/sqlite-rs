//! Text-to-numeric coercion and checked arithmetic (spec 008,
//! Requirement 5). Integer overflow promotes to REAL rather than
//! silently wrapping — the CVE-2025-29087/3277 class.

use crate::record::Value;

/// Locates the longest valid numeric prefix of `s`: optional leading
/// whitespace, optional sign, digits, an optional decimal point and
/// digits, and an optional exponent. Returns the byte range of the
/// literal and whether it is float-shaped (has a `.` or exponent), or
/// `None` if no numeric prefix exists at all.
/// Advances `pos` past a run of bytes matching `pred`, returning the
/// count consumed.
fn skip_while(b: &[u8], pos: &mut usize, pred: impl Fn(u8) -> bool) -> usize {
    let start = *pos;
    while let Some(&c) = b.get(*pos) {
        if !pred(c) {
            break;
        }
        *pos = pos.saturating_add(1);
    }
    pos.saturating_sub(start)
}

fn scan_number_prefix(s: &str) -> Option<(usize, usize, bool)> {
    let b = s.as_bytes();
    let mut i = 0;
    skip_while(b, &mut i, |c| c.is_ascii_whitespace());
    let start = i;
    if matches!(b.get(i), Some(b'+') | Some(b'-')) {
        i = i.saturating_add(1);
    }
    let int_len = skip_while(b, &mut i, |c| c.is_ascii_digit());
    let mut end = i;
    let mut is_float = false;
    if b.get(end) == Some(&b'.') {
        let mut j = end.saturating_add(1);
        let frac_len = skip_while(b, &mut j, |c| c.is_ascii_digit());
        if int_len > 0 || frac_len > 0 {
            is_float = true;
            end = j;
        }
    }
    if int_len == 0 && !is_float {
        return None;
    }
    if matches!(b.get(end), Some(b'e') | Some(b'E')) {
        let mut j = end.saturating_add(1);
        if matches!(b.get(j), Some(b'+') | Some(b'-')) {
            j = j.saturating_add(1);
        }
        let exp_digits = skip_while(b, &mut j, |c| c.is_ascii_digit());
        if exp_digits > 0 {
            end = j;
            is_float = true;
        }
    }
    Some((start, end, is_float))
}

/// Coerces `s` to a numeric `Value` by parsing its longest valid numeric
/// prefix; a non-numeric or empty string coerces to `0`.
pub fn coerce_text_to_numeric(s: &str) -> Value {
    let Some((start, end, is_float)) = scan_number_prefix(s) else {
        return Value::Integer(0);
    };
    let literal = &s[start..end];
    if is_float {
        return literal
            .parse::<f64>()
            .map_or(Value::Integer(0), Value::Real);
    }
    match literal.parse::<i64>() {
        Ok(i) => Value::Integer(i),
        Err(_) => literal
            .parse::<f64>()
            .map_or(Value::Integer(0), Value::Real),
    }
}

fn as_numeric(v: &Value) -> Value {
    match v {
        Value::Integer(_) | Value::Real(_) => v.clone(),
        Value::Text(s) => coerce_text_to_numeric(s),
        Value::Null | Value::Blob(_) => Value::Integer(0),
    }
}

#[allow(clippy::cast_precision_loss)]
fn to_f64(v: &Value) -> f64 {
    match v {
        Value::Integer(i) => *i as f64,
        Value::Real(r) => *r,
        _ => 0.0,
    }
}

fn arith(
    a: &Value,
    b: &Value,
    int_op: fn(i64, i64) -> Option<i64>,
    float_op: fn(f64, f64) -> f64,
) -> Value {
    match (as_numeric(a), as_numeric(b)) {
        (Value::Integer(x), Value::Integer(y)) => match int_op(x, y) {
            Some(v) => Value::Integer(v),
            None => Value::Real(float_op(x as f64, y as f64)),
        },
        (x, y) => Value::Real(float_op(to_f64(&x), to_f64(&y))),
    }
}

/// Adds two values, coercing text operands numerically. Overflow
/// promotes to REAL rather than wrapping.
pub fn checked_add(a: &Value, b: &Value) -> Value {
    arith(a, b, i64::checked_add, |x, y| x + y)
}

/// Subtracts two values, coercing text operands numerically. Overflow
/// promotes to REAL rather than wrapping.
pub fn checked_sub(a: &Value, b: &Value) -> Value {
    arith(a, b, i64::checked_sub, |x, y| x - y)
}

/// Multiplies two values, coercing text operands numerically. Overflow
/// promotes to REAL rather than wrapping.
pub fn checked_mul(a: &Value, b: &Value) -> Value {
    arith(a, b, i64::checked_mul, |x, y| x * y)
}

/// `CAST(... AS INTEGER)`: truncates a REAL toward zero rather than
/// rounding or flooring.
#[allow(clippy::cast_possible_truncation)]
pub fn cast_to_integer(v: &Value) -> i64 {
    match v {
        Value::Integer(i) => *i,
        Value::Real(r) => r.trunc() as i64,
        Value::Text(s) => match coerce_text_to_numeric(s) {
            Value::Integer(i) => i,
            Value::Real(r) => r.trunc() as i64,
            _ => 0,
        },
        Value::Null | Value::Blob(_) => 0,
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn coercion_parses_longest_valid_numeric_prefix() {
        assert_eq!(coerce_text_to_numeric("123abc"), Value::Integer(123));
        assert_eq!(coerce_text_to_numeric("  123  "), Value::Integer(123));
        assert_eq!(coerce_text_to_numeric("abc"), Value::Integer(0));
        assert_eq!(coerce_text_to_numeric(""), Value::Integer(0));
        assert_eq!(coerce_text_to_numeric("0x10"), Value::Integer(0));
        assert_eq!(coerce_text_to_numeric(".5"), Value::Real(0.5));
        assert_eq!(coerce_text_to_numeric("1e3"), Value::Real(1000.0));
    }

    #[test]
    fn arithmetic_matches_oracle_coercion_vectors() {
        assert_eq!(
            checked_add(&Value::Text("123abc".to_string()), &Value::Integer(1)),
            Value::Integer(124)
        );
        assert_eq!(
            checked_add(&Value::Text("  123  ".to_string()), &Value::Integer(1)),
            Value::Integer(124)
        );
        assert_eq!(
            checked_add(&Value::Text("abc".to_string()), &Value::Integer(1)),
            Value::Integer(1)
        );
    }

    #[test]
    fn integer_overflow_promotes_to_real_never_wraps() {
        let max = Value::Integer(i64::MAX);
        match checked_add(&max, &Value::Integer(1)) {
            Value::Real(r) => assert!((r - 9_223_372_036_854_775_808.0).abs() < 1.0),
            other => panic!("expected REAL promotion, got {other:?}"),
        }
        match checked_mul(&max, &Value::Integer(2)) {
            Value::Real(r) => assert!(r > i64::MAX as f64),
            other => panic!("expected REAL promotion, got {other:?}"),
        }
    }

    #[test]
    fn cast_to_integer_truncates_toward_zero() {
        assert_eq!(cast_to_integer(&Value::Real(3.9)), 3);
        assert_eq!(cast_to_integer(&Value::Real(-3.9)), -3);
    }
}
