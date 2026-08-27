// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Spec 002-parser Requirement 5 scenarios: `read_schema` integrates
//! correctly through the corpus harness's own fixture-path resolution.
//! Byte-level DDL-parsing correctness (column extraction, WITHOUT
//! ROWID/STRICT markers, graceful degradation) is already proven by
//! `src/schema/ddl_reader.rs`'s own inline unit tests against these same
//! fixtures.

use sqlite_rs::btree::TableCursor;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::schema::read_schema;
use sqlite_rs::vfs::{UnixVfs, Vfs, VfsPageSource};

use crate::oracle::corpus_dir;

fn schemas_for(family: &str, name: &str) -> Vec<sqlite_rs::schema::TableSchema> {
    let path = corpus_dir().join(family).join(name);
    let vfs = UnixVfs;
    let file = vfs
        .open_read(&path)
        .unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    let source = VfsPageSource::open(&vfs, &path, header.page_size).unwrap();
    let mut cursor = TableCursor::new(source, &header, 1);
    read_schema(&mut cursor, header.text_encoding).unwrap()
}

#[test]
fn table_multipage_schema_has_one_table() {
    let schemas = schemas_for("btrees", "table_multipage.db");
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name, "t");
    assert_eq!(schemas[0].columns, vec!["a", "b"]);
}

#[test]
fn fts5_schema_enumerates_all_shadow_tables() {
    let schemas = schemas_for("features", "fts5.db");
    let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"t"));
    assert!(names.contains(&"t_data"));
    assert!(names.contains(&"t_idx"));
    assert!(names.contains(&"t_content"));
    assert!(names.contains(&"t_docsize"));
    assert!(names.contains(&"t_config"));
}
