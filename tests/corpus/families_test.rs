//! Requirement 3 scenarios: every fixture family present with its expected
//! members. Structural presence only — semantic decode verification lands
//! with the real reader (V1 steps 1-9).

use crate::oracle::corpus_dir;

fn family_files(family: &str) -> Vec<String> {
    let dir = corpus_dir().join(family);
    std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".db"))
        .collect()
}

fn assert_family_contains(family: &str, expected: &[&str]) {
    let files = family_files(family);
    for name in expected {
        assert!(
            files.iter().any(|f| f == name),
            "{family}/ missing {name} (found: {files:?})"
        );
    }
}

#[test]
fn serial_type_family() {
    assert_family_contains("serialtypes", &["values.db"]);
}

#[test]
fn encoding_family() {
    assert_family_contains("encodings", &["utf8.db", "utf16le.db", "utf16be.db"]);
}

#[test]
fn page_geometry_family() {
    assert_family_contains(
        "pagesizes",
        &[
            "page_size_512.db",
            "page_size_65536.db",
            "reserved_bytes_0.db",
            "reserved_bytes_12.db",
        ],
    );
}

#[test]
fn btree_shape_family() {
    assert_family_contains(
        "btrees",
        &[
            "table_single_page.db",
            "table_multipage.db",
            "index.db",
            "without_rowid.db",
            "overflow_single_page.db",
            "overflow_multi_page.db",
        ],
    );
}

#[test]
fn feature_bearing_family() {
    assert_family_contains(
        "features",
        &[
            "autovacuum.db",
            "fts5.db",
            "rtree.db",
            "strict_generated.db",
        ],
    );
}

#[test]
fn invalid_family() {
    assert_family_contains("invalid", &["empty.db", "truncated.db", "magic.db"]);
}
