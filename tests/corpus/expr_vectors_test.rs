//! Spec 008 (value semantics, #77) scenario evidence: structural
//! validation of the oracle-generated vectors under
//! `tests/corpus/expr_vectors/`. These vectors are read by the VDBE
//! expression evaluator's own tests once it exists (phase 3); until then
//! this module is the guarantee that every committed vector is
//! well-formed and that each family covers the cases its spec scenarios
//! claim.
//!
//! Vectors are one JSON object per line, but this module deliberately
//! avoids adding a `serde_json` dependency for a docs-only spec ticket —
//! checks are line-oriented substring/count assertions rather than a
//! full JSON parse.

use std::path::{Path, PathBuf};

fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/expr_vectors")
}

fn read_lines(family: &str) -> Vec<String> {
    let path = vectors_dir().join(format!("{family}.jsonl"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let lines: Vec<String> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    assert!(!lines.is_empty(), "{family}.jsonl has no vectors");
    for line in &lines {
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "{family}.jsonl has a malformed line: {line}"
        );
    }
    lines
}

#[test]
fn affinity_vectors_cover_all_five_affinity_classes() {
    let vectors = read_lines("affinity");
    // BLOB affinity never converts, so a '1.5' text literal stays text;
    // INTEGER/NUMERIC/REAL all coerce '1.5' to real; TEXT stays text.
    assert!(
        vectors
            .iter()
            .any(|v| v.contains(r#""affinity_probe_stored_type": "real""#)),
        "expected a NUMERIC/INTEGER/REAL-affinity probe"
    );
    assert!(
        vectors
            .iter()
            .any(|v| v.contains(r#""affinity_probe_stored_type": "text""#)),
        "expected a TEXT/BLOB-affinity probe"
    );
}

#[test]
fn affinity_vectors_include_declared_type_rules_table_entries() {
    let vectors = read_lines("affinity");
    for expected in ["INTEGER", "TEXT", "BLOB", "REAL", "NUMERIC"] {
        assert!(
            vectors
                .iter()
                .any(|v| v.contains(&format!(r#""declared_type": "{expected}""#))),
            "affinity.jsonl missing declared_type {expected:?}"
        );
    }
    assert!(
        vectors.iter().any(|v| v.contains(r#""declared_type": """#)),
        "affinity.jsonl missing the no-declared-type (blank) case"
    );
}

#[test]
fn comparison_vectors_cover_null_numeric_text_blob_ordering() {
    let vectors = read_lines("comparison");
    assert!(
        vectors.iter().any(|v| v.contains("NULL <")),
        "missing NULL-ordering vector"
    );
    assert!(
        vectors.iter().any(|v| v.contains(r"< 'a'")),
        "missing numeric-vs-text vector"
    );
    assert!(
        vectors.iter().any(|v| v.contains(r"< x'")),
        "missing text-vs-blob vector"
    );
    assert!(
        vectors.iter().any(|v| v.contains("= 2.0")),
        "missing integer/real class-merge vector"
    );
}

#[test]
fn collation_vectors_cover_binary_nocase_rtrim() {
    let vectors = read_lines("collation");
    for collation in ["COLLATE BINARY", "COLLATE NOCASE", "COLLATE RTRIM"] {
        assert!(
            vectors.iter().any(|v| v.contains(collation)),
            "collation.jsonl missing a {collation} vector"
        );
    }
    // NOCASE is documented ASCII-only case folding, not Unicode — the
    // divergence trap this spec calls out explicitly.
    assert!(
        vectors.iter().any(|v| v.contains(r"ß") || v.contains(r"é")),
        "collation.jsonl missing a non-ASCII NOCASE divergence vector"
    );
}

#[test]
fn null_vectors_cover_three_valued_logic_and_is_vs_eq() {
    let vectors = read_lines("null");
    assert!(
        vectors
            .iter()
            .any(|v| v.contains(r#""expr": "NULL AND 0""#)),
        "missing three-valued-logic AND-with-false vector"
    );
    assert!(
        vectors.iter().any(|v| v.contains(r#""expr": "NULL OR 1""#)),
        "missing three-valued-logic OR-with-true vector"
    );
    assert!(
        vectors
            .iter()
            .any(|v| v.contains(r#""expr": "NULL IS NULL""#)),
        "missing IS NULL vector"
    );
    assert!(
        vectors.iter().any(|v| v.contains(r#""expr": "1 = NULL""#)),
        "missing propagation-through-= vector"
    );
}

#[test]
fn coercion_vectors_cover_text_parsing_and_overflow_promotion() {
    let vectors = read_lines("coercion");
    assert!(
        vectors
            .iter()
            .any(|v| v.contains(r#""expr": "'123abc' + 1""#)),
        "missing partial-numeric-prefix parse vector"
    );
    let overflow = vectors
        .iter()
        .find(|v| v.contains(r#""expr": "9223372036854775807 + 1""#))
        .expect("missing integer-overflow REAL-promotion vector");
    // Overflowing arithmetic must promote to REAL, never silently wrap —
    // the CVE-2025-29087/3277 class the issue calls out.
    assert!(
        overflow.contains(r#""type": "real""#),
        "integer overflow must promote to REAL, not wrap: {overflow}"
    );
}

#[test]
fn function_vectors_cover_null_propagation_and_the_coalesce_exception() {
    let vectors = read_lines("functions");
    let length_null = vectors
        .iter()
        .find(|v| v.contains(r#""expr": "length(NULL)""#))
        .expect("missing length(NULL) vector");
    assert!(
        length_null.contains(r#""type": "null""#),
        "length(NULL) must propagate NULL: {length_null}"
    );
    let coalesce = vectors
        .iter()
        .find(|v| v.contains(r#""expr": "coalesce(NULL,NULL,3)""#))
        .expect("missing coalesce NULL-skipping vector");
    assert!(
        coalesce.contains(r#""value_quoted": "3""#),
        "coalesce must skip NULLs to find the first non-NULL: {coalesce}"
    );
}

#[test]
fn function_vectors_cover_ascii_only_case_folding() {
    let vectors = read_lines("functions");
    assert!(
        vectors
            .iter()
            .any(|v| v.contains(r#""expr": "upper('café')""#) && v.contains("CAFé")),
        "upper() must be ASCII-only — 'é' must not fold to 'É'"
    );
}

#[test]
fn function_vectors_cover_substr_negative_and_zero_index_rules() {
    let vectors = read_lines("functions");
    for expected in [
        r#""expr": "substr('hello',-3)""#,
        r#""expr": "substr('hello',0)""#,
        r#""expr": "substr('hello',2,-1)""#,
        r#""expr": "substr('hello',-100,2)""#,
    ] {
        assert!(
            vectors.iter().any(|v| v.contains(expected)),
            "functions.jsonl missing substr vector {expected}"
        );
    }
}

#[test]
fn function_vectors_quote_output_is_byte_exact_with_escaped_quotes() {
    let vectors = read_lines("functions");
    let quoted = vectors
        .iter()
        .find(|v| v.contains(r#""expr": "quote('it''s')""#))
        .expect("missing quote() single-quote-escaping vector");
    assert!(
        quoted.contains(r"'''it''''s'''"),
        "quote() must double embedded single quotes byte-exact: {quoted}"
    );
}
