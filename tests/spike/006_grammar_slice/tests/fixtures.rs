use grammar_slice_spike::{Outcome, parse, split_statements};

fn load(name: &str) -> String {
    let path = format!("{}/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path, e))
}

#[test]
fn v2_valid_statements_are_accepted() {
    let content = load("v2_valid.sql");
    for stmt in split_statements(&content) {
        match parse(stmt) {
            Outcome::Accepted(_) => {}
            other => panic!("expected Accepted for {:?}, got {:?}", stmt, other),
        }
    }
}

#[test]
fn out_of_slice_statements_are_unsupported_not_syntax_errors() {
    let content = load("unsupported.sql");
    for stmt in split_statements(&content) {
        match parse(stmt) {
            Outcome::Unsupported(_) => {}
            other => panic!("expected Unsupported for {:?}, got {:?}", stmt, other),
        }
    }
}

#[test]
fn malformed_statements_are_syntax_errors() {
    let content = load("invalid.sql");
    for stmt in split_statements(&content) {
        match parse(stmt) {
            Outcome::SyntaxError(_) => {}
            other => panic!("expected SyntaxError for {:?}, got {:?}", stmt, other),
        }
    }
}

#[test]
fn three_way_outcomes_are_pairwise_distinct() {
    // Sanity check on the enum itself, not just the fixture files: accepted,
    // unsupported and syntax-error are structurally different variants, so a
    // caller can always tell them apart (exit criterion: "3-way outcome
    // parity").
    let accepted = parse("SELECT a FROM t");
    let unsupported = parse("INSERT INTO t VALUES (1)");
    let syntax_error = parse("SELECT FROM t");

    assert!(matches!(accepted, Outcome::Accepted(_)));
    assert!(matches!(unsupported, Outcome::Unsupported(_)));
    assert!(matches!(syntax_error, Outcome::SyntaxError(_)));
}
