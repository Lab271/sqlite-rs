//! Type affinity: the five-way storage preference steered by a column's
//! declared type (spec 008, Requirement 1;
//! <https://www.sqlite.org/datatype3.html> §3.1).

use crate::record::Value;

/// A column's storage-class preference, derived from its declared type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affinity {
    Text,
    Numeric,
    Integer,
    Real,
    Blob,
}

/// Derives affinity from a declared type string per the datatype3.html
/// substring rules, checked in order: INT, then CHAR/CLOB/TEXT, then
/// BLOB or no declared type, then REAL/FLOA/DOUB, else NUMERIC.
pub fn affinity_of(declared_type: &str) -> Affinity {
    let upper = declared_type.to_ascii_uppercase();
    if upper.contains("INT") {
        Affinity::Integer
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        Affinity::Text
    } else if upper.contains("BLOB") || upper.is_empty() {
        Affinity::Blob
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        Affinity::Real
    } else {
        Affinity::Numeric
    }
}

/// Converts a well-formed numeric-text literal to its lossless numeric
/// representation for NUMERIC/INTEGER/REAL affinities. TEXT and BLOB
/// affinities never convert (spec 008, Requirement 1).
pub fn apply_affinity(value: &mut Value, affinity: Affinity) {
    if matches!(affinity, Affinity::Text | Affinity::Blob) {
        return;
    }
    if let Value::Text(s) = value {
        if let Some(numeric) = parse_well_formed_number(s) {
            *value = numeric;
        }
    }
}

/// Parses `s` as a number only if the *entire* trimmed string is a valid
/// numeric literal (unlike coercion's longest-prefix rule).
fn parse_well_formed_number(s: &str) -> Option<Value> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(i) = trimmed.parse::<i64>() {
        return Some(Value::Integer(i));
    }
    if let Ok(r) = trimmed.parse::<f64>() {
        if r.is_finite() {
            return Some(Value::Real(r));
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn vectors() -> String {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/expr_vectors/affinity.jsonl");
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn affinity_of_matches_oracle_declared_types() {
        // Each (declared_type, expected affinity) pair mirrors a row in
        // affinity.jsonl's declared_type column.
        let cases = [
            ("INTEGER", Affinity::Integer),
            ("INT", Affinity::Integer),
            ("TINYINT", Affinity::Integer),
            ("UNSIGNED BIG INT", Affinity::Integer),
            ("INT2", Affinity::Integer),
            ("TEXT", Affinity::Text),
            ("CHARACTER(20)", Affinity::Text),
            ("VARCHAR(255)", Affinity::Text),
            ("VARYING CHARACTER(255)", Affinity::Text),
            ("NCHAR(55)", Affinity::Text),
            ("NATIVE CHARACTER(70)", Affinity::Text),
            ("NVARCHAR(100)", Affinity::Text),
            ("CLOB", Affinity::Text),
            ("BLOB", Affinity::Blob),
            ("", Affinity::Blob),
            ("REAL", Affinity::Real),
            ("DOUBLE", Affinity::Real),
            ("DOUBLE PRECISION", Affinity::Real),
            ("FLOAT", Affinity::Real),
            ("NUMERIC", Affinity::Numeric),
            ("DECIMAL(10,5)", Affinity::Numeric),
            ("BOOLEAN", Affinity::Numeric),
            ("DATE", Affinity::Numeric),
            ("DATETIME", Affinity::Numeric),
            // "POINT" contains the substring "INT" — a documented SQLite
            // divergence trap (datatype3.html): it gets INTEGER
            // affinity, not NUMERIC, despite not being a numeric type.
            ("POINT", Affinity::Integer),
            ("STRING", Affinity::Numeric),
        ];
        for (declared, expected) in cases {
            assert_eq!(
                affinity_of(declared),
                expected,
                "affinity_of({declared:?}) mismatch"
            );
        }
    }

    #[test]
    fn oracle_vectors_stored_type_matches_affinity_conversion() {
        // BLOB affinity (declared BLOB or blank) never converts '1.5' —
        // stays text; every other affinity converts it to a REAL, per
        // affinity.jsonl's affinity_probe_stored_type column.
        for line in vectors().lines().filter(|l| !l.is_empty()) {
            let declared = line
                .split(r#""declared_type": ""#)
                .nth(1)
                .unwrap()
                .trim_end_matches("\"}")
                .to_string();
            let expect_text = line.contains(r#""affinity_probe_stored_type": "text""#);
            let affinity = affinity_of(&declared);
            let mut value = Value::Text("1.5".to_string());
            apply_affinity(&mut value, affinity);
            let is_text = matches!(value, Value::Text(_));
            assert_eq!(
                is_text, expect_text,
                "declared_type {declared:?} -> affinity {affinity:?}: expected text={expect_text}"
            );
        }
    }
}
