//! Property-based coverage for the value-semantics kernel (spec 008,
//! Requirements 1 and 5): affinity and numeric-coercion idempotence.
//!
//! Lives outside `src/` for the same reason as `tokenizer_proptest.rs`:
//! `proptest!`'s macro expansion isn't in the qualified subset's
//! curated macro allowlist (issue #23 / `make mvl-limit`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use proptest::prelude::*;
use sqlite_rs::record::Value;
use sqlite_rs::vdbe::{apply_affinity, coerce_text_to_numeric, Affinity};

fn arb_affinity() -> impl Strategy<Value = Affinity> {
    prop_oneof![
        Just(Affinity::Text),
        Just(Affinity::Numeric),
        Just(Affinity::Integer),
        Just(Affinity::Real),
        Just(Affinity::Blob),
    ]
}

proptest! {
    /// Applying affinity twice must equal applying it once: the second
    /// pass sees a value that is already in its converted (or
    /// deliberately unconverted, for TEXT/BLOB) form and must be a no-op.
    #[test]
    fn affinity_is_idempotent(s in ".*", affinity in arb_affinity()) {
        let mut once = Value::Text(s);
        apply_affinity(&mut once, affinity);
        let mut twice = once.clone();
        apply_affinity(&mut twice, affinity);
        prop_assert_eq!(once, twice);
    }

    /// Coercing an already-numeric text value must reproduce the same
    /// number: printing the coerced value back to text and coercing
    /// again is a fixed point.
    #[test]
    fn coerce_text_to_numeric_is_idempotent_on_numeric_text(i in any::<i64>()) {
        let s = i.to_string();
        let once = coerce_text_to_numeric(&s);
        prop_assert!(matches!(once, Value::Integer(_) | Value::Real(_)));
        let roundtrip = match &once {
            Value::Integer(n) => coerce_text_to_numeric(&n.to_string()),
            Value::Real(r) => coerce_text_to_numeric(&r.to_string()),
            _ => unreachable!(),
        };
        prop_assert_eq!(once, roundtrip);
    }

    /// Coercing a real (fractional) numeric string round-trips through
    /// its own text rendering.
    #[test]
    fn coerce_text_to_numeric_is_idempotent_on_real_text(r in any::<f64>().prop_filter("finite", |r| r.is_finite())) {
        let s = r.to_string();
        let once = coerce_text_to_numeric(&s);
        prop_assert!(matches!(once, Value::Integer(_) | Value::Real(_)));
        let twice = match &once {
            Value::Integer(n) => coerce_text_to_numeric(&n.to_string()),
            Value::Real(n) => coerce_text_to_numeric(&n.to_string()),
            _ => unreachable!(),
        };
        prop_assert_eq!(once, twice);
    }
}
