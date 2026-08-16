//! Tier 1 — QUERY CORE (spec 001-architecture Tier Model, `plan.md` Core
//! Definition): single-table SELECT, core scalar functions, affinity,
//! built-in collations. Planner droppable (full scans are correct);
//! SELECT core itself is not.
//!
//! Green clauses today are the tokenizer (0.5.0); everything else is a
//! `#[ignore]` stub tagged with the V-block/ticket that will flip it, per
//! CLAUDE.md's tier-stub-flip convention.

use sqlite_rs::parser::tokenizer::Tokenizer;

/// The tokenizer must round-trip every token it produces and never
/// panic on malformed input — this is the one Tier 1 clause already
/// green (0.5.0), ahead of the rest of QUERY CORE.
#[test]
fn t1_tokenizer_roundtrip_never_panics() {
    for input in ["SELECT * FROM t WHERE a = 1", "", "'unterminated", "\0\0\0"] {
        let _ = Tokenizer::tokenize(input);
    }
}

#[test]
#[ignore = "V2 phase 1 SELECT-core parser landed (#61); acceptance/rejection vectors pending (#69 follow-up)"]
fn t1_select_core_accepts_and_rejects() {
    unimplemented!()
}

/// V2 phase 2 — value-semantics kernel (#78): affinity derivation and
/// conversion, cross-type comparison order, and collation all behave
/// per spec 008. Full oracle-vector coverage lives in
/// `tests/corpus/expr_vectors_test.rs::{affinity,comparison,collation}_vectors_*`.
#[test]
fn t1_expression_kernel_affinity_and_collation_vectors() {
    use std::cmp::Ordering;

    use sqlite_rs::record::Value;
    use sqlite_rs::vdbe::{
        affinity_of, apply_affinity, compare, compare_text, Affinity, Collation,
    };

    assert_eq!(affinity_of("INTEGER"), Affinity::Integer);
    assert_eq!(affinity_of("VARCHAR(10)"), Affinity::Text);
    assert_eq!(affinity_of(""), Affinity::Blob);

    let mut v = Value::Text("1.5".to_string());
    apply_affinity(&mut v, Affinity::Real);
    assert_eq!(v, Value::Real(1.5));

    assert_eq!(
        compare(&Value::Null, &Value::Integer(0), Collation::Binary),
        Ordering::Less
    );
    assert_eq!(
        compare(
            &Value::Integer(1),
            &Value::Text("a".into()),
            Collation::Binary
        ),
        Ordering::Less
    );

    assert_eq!(compare_text("I", "i", Collation::NoCase), Ordering::Equal);
}

/// V2 phase 2 — scalar function core (#79): dispatch through the
/// registry never panics, NULL propagates for the general case, and
/// coalesce is the documented NULL-propagation exception. Full
/// oracle-vector coverage lives in
/// `tests/corpus/expr_vectors_test.rs::function_vectors_*`.
#[test]
fn t1_scalar_functions_match_oracle() {
    use sqlite_rs::record::Value;
    use sqlite_rs::vdbe::call_function;

    assert_eq!(call_function("length", &[Value::Null]), Ok(Value::Null));
    assert_eq!(
        call_function("coalesce", &[Value::Null, Value::Null, Value::Integer(3)]),
        Ok(Value::Integer(3))
    );
    assert!(call_function("nope", &[]).is_err());
}

#[test]
#[ignore = "V2 phase 3 — single-table SELECT execution"]
fn t1_single_table_where_matches_oracle() {
    unimplemented!()
}

#[test]
#[ignore = "V2 phase 3 — EXPLAIN bytecode output"]
fn t1_explain_prints_bytecode() {
    unimplemented!()
}
