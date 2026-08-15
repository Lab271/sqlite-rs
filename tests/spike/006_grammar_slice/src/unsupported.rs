//! "Unsupported" classification: the third outcome the spike falsification
//! criterion requires alongside accept/syntax-error.
//!
//! The sliced grammar has no productions at all for V3/V4+ constructs, so it
//! cannot distinguish "well-formed SQL outside this slice" from "malformed
//! SQL" structurally -- both come back as a `Token`-level syntax error from
//! pomelo. Instead, a cheap keyword sniff runs first: if the input's first
//! keyword or a scan of its keywords match a known out-of-slice feature, that
//! is reported as "unsupported" with the V-block it belongs to; anything else
//! that fails to parse is a genuine syntax error.
//!
//! This is intentionally coarse (no full lexing, just a whitespace/word
//! split) -- good enough to answer the falsification question ("can
//! unsupported vs invalid be distinguished at all") without building a
//! second tokenizer.

pub fn classify_unsupported(sql: &str) -> Option<&'static str> {
    let upper = sql.to_ascii_uppercase();
    let words: Vec<&str> = upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .collect();
    let first = words.first().copied().unwrap_or("");

    // V3: other DML/DDL statement kinds.
    match first {
        "INSERT" => return Some("INSERT (V3)"),
        "UPDATE" => return Some("UPDATE (V3)"),
        "DELETE" => return Some("DELETE (V3)"),
        "CREATE" => return Some("CREATE TABLE/INDEX (V3)"),
        "DROP" => return Some("DROP TABLE/INDEX (V3)"),
        "WITH" => return Some("WITH/CTE (V4)"),
        _ => {}
    }

    if first != "SELECT" {
        return None;
    }

    // V4: features inside a SELECT that the V2 slice's grammar has no
    // production for at all.
    let v4_markers: &[(&str, &str)] = &[
        ("JOIN", "join (V4)"),
        ("UNION", "compound select: UNION (V4)"),
        ("INTERSECT", "compound select: INTERSECT (V4)"),
        ("EXCEPT", "compound select: EXCEPT (V4)"),
        ("GROUP", "GROUP BY (V4)"),
        ("HAVING", "HAVING (V4)"),
    ];
    for (marker, label) in v4_markers {
        if words.iter().any(|w| w == marker) {
            return Some(label);
        }
    }

    // A second SELECT keyword anywhere after the first word is a subquery
    // (scalar, FROM-clause, or EXISTS) -- V4, no production for it here.
    if words.iter().skip(1).any(|w| *w == "SELECT") {
        return Some("subquery (V4)");
    }

    None
}
