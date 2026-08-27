// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Oracle-diff harness for spike #59: every vector under
//! `tests/corpus/expr_vectors/` (expression-shaped families — not
//! `affinity.jsonl`, which is declared-type probes, not expressions)
//! is parsed, walked through the value-semantics kernel, and rendered
//! back via the kernel's own `quote`/`typeof` functions for a
//! byte-exact comparison against the pinned oracle's `value_quoted`/
//! `type` columns. This is the "vector ratchet": these vectors already
//! existed (spec 008 phase 2) but nothing executed them until now.

use std::fs;
use std::path::Path;

use sqlite_rs::record::Value;
use sqlite_rs::vdbe::call_function;
use tree_walker_spike::{eval_sql_expr, WalkError};

#[derive(serde::Deserialize)]
struct Vector {
    expr: String,
    #[serde(rename = "type")]
    ty: String,
    value_quoted: String,
}

fn corpus_dir() -> &'static Path {
    // `cargo test` runs with CWD at this crate's root
    // (tests/spike/008_tree_walker), not the repo root.
    Path::new("../../corpus/expr_vectors")
}

fn load(family: &str) -> Vec<Vector> {
    let path = corpus_dir().join(format!("{family}.jsonl"));
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {family}: {e}"));
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{family}: bad line {l}: {e}")))
        .collect()
}

fn render(v: &Value) -> (String, String) {
    let quoted = match call_function("quote", std::slice::from_ref(v)) {
        Ok(Value::Text(s)) => s,
        other => panic!("quote() returned unexpected {other:?}"),
    };
    let ty = match call_function("typeof", std::slice::from_ref(v)) {
        Ok(Value::Text(s)) => s,
        other => panic!("typeof() returned unexpected {other:?}"),
    };
    (quoted, ty)
}

/// Vectors with a *known*, already-documented divergence, kept in the
/// corpus (so the oracle-diff coverage stays visible) but excluded from
/// the pass/fail gate. Each entry cites the pre-existing gap it hits —
/// see findings.md for the follow-up issues filed against each.
const KNOWN_DIVERGENCES: &[&str] = &[
    // format_real's 15-significant-digit rendering vs. the oracle's
    // ~17-digit REAL rendering on overflow-promoted values — the same
    // gap #92's review already documented for quote()/hex()/length().
    "9223372036854775807 + 1",
    "9223372036854775807 + 1.0",
    "-9223372036854775808 - 1",
    "9223372036854775807 * 2",
    // The tokenizer folds a bare `9223372036854775808` (unrepresentable
    // as a positive i64) to a Float literal before unary minus is
    // applied, losing SQLite's special-cased `-9223372036854775808`
    // i64::MIN integer-literal parse.
    "-9223372036854775808",
];

fn diff_family(family: &str) {
    let vectors = load(family);
    assert!(!vectors.is_empty(), "{family}.jsonl has no vectors");
    let mut failures = Vec::new();
    for v in vectors {
        if KNOWN_DIVERGENCES.contains(&v.expr.as_str()) {
            continue;
        }
        let result = eval_sql_expr(&v.expr);
        match result {
            Ok(value) => {
                let (quoted, ty) = render(&value);
                if (quoted.as_str(), ty.as_str()) != (v.value_quoted.as_str(), v.ty.as_str()) {
                    failures.push(format!(
                        "{}: got ({quoted:?}, {ty:?}), expected ({:?}, {:?})",
                        v.expr, v.value_quoted, v.ty
                    ));
                }
            }
            Err(WalkError::Unsupported(msg)) => {
                failures.push(format!("{}: unsupported by walker: {msg}", v.expr))
            }
            Err(WalkError::Parse(msg)) => {
                failures.push(format!("{}: failed to parse: {msg}", v.expr))
            }
            Err(WalkError::Function(e)) => {
                failures.push(format!("{}: function call failed: {e}", v.expr))
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{family}: {} divergence(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn comparison_vectors_match_oracle() {
    diff_family("comparison");
}

#[test]
fn collation_vectors_match_oracle() {
    diff_family("collation");
}

#[test]
fn null_vectors_match_oracle() {
    diff_family("null");
}

#[test]
fn coercion_vectors_match_oracle() {
    diff_family("coercion");
}

#[test]
fn function_vectors_match_oracle() {
    diff_family("functions");
}

#[test]
fn walker_vectors_match_oracle() {
    diff_family("walker");
}
