// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Cross-type comparison order (spec 008, Requirement 2): NULL < numeric
//! < text < blob, with INTEGER and REAL merged into one numeric class.

use std::cmp::Ordering;

use crate::record::{compare_text, Collation, Value};

#[inline]
fn value_rank(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Integer(_) | Value::Real(_) => 1,
        Value::Text(_) => 2,
        Value::Blob(_) => 3,
    }
}

/// Compares an `i64` against an `f64` the way SQLite does: a straight
/// `as f64` cast loses precision near `i64::MAX`/`MIN`, which would
/// wrongly report `i64::MAX == (i64::MAX as f64)` even though the
/// nearest representable double for that magnitude has already rounded
/// past it. Mirrors sqlite3IntFloatCompare (util.c).
fn compare_int_real(i: i64, r: f64) -> Ordering {
    if r.is_nan() {
        return Ordering::Greater;
    }
    if r < -9_223_372_036_854_775_808.0 {
        return Ordering::Greater;
    }
    if r >= 9_223_372_036_854_775_808.0 {
        return Ordering::Less;
    }
    #[allow(clippy::cast_possible_truncation)]
    let y = r as i64;
    if i < y {
        return Ordering::Less;
    }
    if i > y {
        return Ordering::Greater;
    }
    #[allow(clippy::cast_precision_loss)]
    let s = i as f64;
    s.partial_cmp(&r).unwrap_or(Ordering::Equal)
}

/// Total order over `Value`s: NULL < numeric < text < blob, per spec 008
/// Requirement 2. `collation` governs text-vs-text comparisons only.
#[inline]
pub fn compare(a: &Value, b: &Value, collation: Collation) -> Ordering {
    let (ra, rb) = (value_rank(a), value_rank(b));
    if ra != rb {
        return ra.cmp(&rb);
    }
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Integer(x), Value::Integer(y)) => x.cmp(y),
        (Value::Real(x), Value::Real(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Integer(x), Value::Real(y)) => compare_int_real(*x, *y),
        (Value::Real(x), Value::Integer(y)) => compare_int_real(*y, *x).reverse(),
        (Value::Text(x), Value::Text(y)) => compare_text(x, y, collation),
        (Value::Blob(x), Value::Blob(y)) => x.cmp(y),
        _ => Ordering::Equal, // unreachable: value_rank already separated these
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_is_lower_than_every_other_class() {
        for other in [
            Value::Integer(1),
            Value::Text("a".to_string().into()),
            Value::Blob(vec![0].into()),
        ] {
            assert_eq!(
                compare(&Value::Null, &other, Collation::Binary),
                Ordering::Less
            );
        }
    }

    #[test]
    fn numeric_sorts_below_text_below_blob() {
        let one = Value::Integer(1);
        let a = Value::Text("a".to_string().into());
        let blob = Value::Blob(vec![0].into());
        assert_eq!(compare(&one, &a, Collation::Binary), Ordering::Less);
        assert_eq!(compare(&one, &blob, Collation::Binary), Ordering::Less);
        assert_eq!(compare(&a, &blob, Collation::Binary), Ordering::Less);
    }

    #[test]
    fn integer_and_real_merge_into_one_numeric_class() {
        assert_eq!(
            compare(&Value::Integer(2), &Value::Real(2.0), Collation::Binary),
            Ordering::Equal
        );
        // i64::MAX as a REAL rounds up past the exact integer value.
        assert_eq!(
            compare(
                &Value::Integer(9_223_372_036_854_775_807),
                &Value::Real(9_223_372_036_854_775_807.0),
                Collation::Binary
            ),
            Ordering::Less
        );
    }
}
