// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Spike 005 (#12): export every user table of a SQLite file to CSV.
//! Throwaway experiment — reuses the real crate's `DatabaseHeader`,
//! `Vfs`/`VfsFile`, `record::decode_record`/`decode_varint`, and prototypes
//! the new logic steps 4/5/7 will need for real: interior-node traversal,
//! overflow-chain reassembly, sqlite_master enumeration, and a minimal DDL
//! column-name extractor. See findings.md.

use std::path::{Path, PathBuf};

use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::record::{decode_record, decode_varint, TextEncoding, Value};
use sqlite_rs::vfs::{UnixVfs, Vfs, VfsFile};

struct Row {
    rowid: i64,
    payload: Vec<u8>,
}

struct SchemaEntry {
    kind: String,
    name: String,
    rootpage: i64,
    sql: String,
}

fn read_page(file: &dyn VfsFile, page_num: u32, page_size: u32) -> Vec<u8> {
    let mut buf = vec![0u8; page_size as usize];
    let offset = (page_num as u64 - 1) * page_size as u64;
    let n = file.read_at(&mut buf, offset).expect("read_at failed");
    assert_eq!(n, buf.len(), "short read on page {page_num}");
    buf
}

/// SQLite's overflow local-size formula (fileformat2.html "Cell Payload
/// Overflow"). `usable_size` = page_size - reserved_space.
fn local_payload_size(usable_size: u32, payload_len: u32) -> u32 {
    let max_local = usable_size - 35;
    if payload_len <= max_local {
        return payload_len;
    }
    let min_local = ((usable_size - 12) * 32 / 255) - 23;
    let k = min_local + (payload_len - min_local) % (usable_size - 4);
    if k <= max_local {
        k
    } else {
        min_local
    }
}

fn reassemble_payload(
    file: &dyn VfsFile,
    header: &DatabaseHeader,
    cell: &[u8],
    payload_len: u32,
) -> Vec<u8> {
    let usable_size = header.usable_page_size();
    let local_size = local_payload_size(usable_size, payload_len) as usize;
    let mut result = cell[..local_size].to_vec();
    if local_size as u32 == payload_len {
        return result;
    }

    let mut overflow_page =
        u32::from_be_bytes(cell[local_size..local_size + 4].try_into().unwrap());
    let mut remaining = payload_len as usize - local_size;
    let available = usable_size as usize - 4;
    while remaining > 0 {
        assert_ne!(overflow_page, 0, "overflow chain ended early");
        let page = read_page(file, overflow_page, header.page_size);
        let next = u32::from_be_bytes(page[0..4].try_into().unwrap());
        let take = remaining.min(available);
        result.extend_from_slice(&page[4..4 + take]);
        remaining -= take;
        overflow_page = next;
    }
    result
}

/// Depth-first walk of a table b-tree (page types 0x05 interior / 0x0d
/// leaf), returning rows in ascending rowid order.
fn walk_table_btree(
    file: &dyn VfsFile,
    header: &DatabaseHeader,
    root_page: u32,
    out: &mut Vec<Row>,
) {
    let page = read_page(file, root_page, header.page_size);
    let header_start = if root_page == 1 { 100 } else { 0 };
    let page_type = page[header_start];
    let num_cells = u16::from_be_bytes([page[header_start + 3], page[header_start + 4]]) as usize;

    match page_type {
        0x0d => {
            let cell_ptr_base = header_start + 8;
            for i in 0..num_cells {
                let ptr_off = cell_ptr_base + 2 * i;
                let cell_start = u16::from_be_bytes([page[ptr_off], page[ptr_off + 1]]) as usize;
                let cell = &page[cell_start..];
                let (payload_len, n1) = decode_varint(cell).expect("payload len varint");
                let (rowid, n2) = decode_varint(&cell[n1..]).expect("rowid varint");
                let payload =
                    reassemble_payload(file, header, &cell[n1 + n2..], payload_len as u32);
                out.push(Row {
                    rowid: rowid as i64,
                    payload,
                });
            }
        }
        0x05 => {
            let cell_ptr_base = header_start + 12;
            let rightmost = u32::from_be_bytes(
                page[header_start + 8..header_start + 12]
                    .try_into()
                    .unwrap(),
            );
            for i in 0..num_cells {
                let ptr_off = cell_ptr_base + 2 * i;
                let cell_start = u16::from_be_bytes([page[ptr_off], page[ptr_off + 1]]) as usize;
                let cell = &page[cell_start..];
                let child_page = u32::from_be_bytes(cell[0..4].try_into().unwrap());
                walk_table_btree(file, header, child_page, out);
            }
            walk_table_btree(file, header, rightmost, out);
        }
        other => panic!("unexpected table b-tree page type {other:#x} on page {root_page}"),
    }
}

fn decode_schema(file: &dyn VfsFile, header: &DatabaseHeader) -> Vec<SchemaEntry> {
    let mut rows = Vec::new();
    walk_table_btree(file, header, 1, &mut rows);
    rows.iter()
        .map(|r| {
            let values =
                decode_record(&r.payload, header.text_encoding).expect("sqlite_master record");
            SchemaEntry {
                kind: text_value(&values[0]),
                name: text_value(&values[1]),
                rootpage: int_value(&values[3]),
                sql: text_value(&values[4]),
            }
        })
        .collect()
}

fn text_value(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Null => String::new(),
        other => panic!("expected text, got {other:?}"),
    }
}

fn int_value(v: &Value) -> i64 {
    match v {
        Value::Integer(i) => *i,
        other => panic!("expected integer, got {other:?}"),
    }
}

/// Minimal DDL column-definition extraction: everything between the
/// outermost parens of a `CREATE TABLE` statement, split on top-level
/// commas. Deliberately naive — quoted/bracketed identifiers and inline
/// constraints are not handled; see findings.md.
fn extract_column_defs(sql: &str) -> Vec<String> {
    let start = sql.find('(').expect("CREATE TABLE without '('");
    let mut depth = 0i32;
    let mut end = start;
    for (i, c) in sql[start..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let inner = &sql[start + 1..end];

    let mut columns = Vec::new();
    let mut depth = 0i32;
    let mut part_start = 0usize;
    let bytes = inner.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                columns.push(inner[part_start..i].trim().to_string());
                part_start = i + 1;
            }
            _ => {}
        }
    }
    columns.push(inner[part_start..].trim().to_string());
    columns
}

fn column_names(defs: &[String]) -> Vec<String> {
    defs.iter()
        .map(|def| {
            def.split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(['"', '`', '['].as_ref())
                .trim_matches([']'].as_ref())
                .to_string()
        })
        .collect()
}

/// SQLite's rowid-alias optimization: a column declared exactly `INTEGER
/// PRIMARY KEY` (not `INT`, not composite) is NOT stored in the record —
/// it's encoded as NULL, and the reader must substitute the cell's own
/// rowid. Naive detection: first column def containing "integer" and
/// "primary key" as whole words, case-insensitive. See findings.md.
fn rowid_alias_index(defs: &[String]) -> Option<usize> {
    defs.iter().position(|def| {
        let upper = def.to_ascii_uppercase();
        upper.contains("INTEGER") && upper.contains("PRIMARY KEY")
    })
}

fn is_virtual_table(sql: &str) -> bool {
    sql.trim_start()
        .to_ascii_uppercase()
        .starts_with("CREATE VIRTUAL TABLE")
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn render_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Text(s) => csv_escape(s),
        Value::Blob(b) => {
            let hex: String = b.iter().map(|byte| format!("{byte:02X}")).collect();
            format!("X'{hex}'")
        }
    }
}

fn write_csv(
    input_path: &Path,
    table: &str,
    columns: &[String],
    rowid_alias: Option<usize>,
    rows: &[Row],
    encoding: TextEncoding,
) {
    let stem = input_path
        .file_stem()
        .expect("input path has no file stem")
        .to_string_lossy();
    let out_path: PathBuf = input_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{table}_{stem}.csv"));

    let mut out = String::new();
    out.push_str(&columns.join(","));
    out.push('\n');
    for row in rows {
        let values = decode_record(&row.payload, encoding).expect("row record");
        let rendered: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                if rowid_alias == Some(i) {
                    row.rowid.to_string()
                } else {
                    render_value(v)
                }
            })
            .collect();
        out.push_str(&rendered.join(","));
        out.push('\n');
    }
    std::fs::write(&out_path, out).expect("writing csv");
    println!("wrote {} ({} rows)", out_path.display(), rows.len());
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: spike-csv-export <path.db>");
    let path = Path::new(&path);

    let vfs = UnixVfs;
    let file = vfs.open_read(path).expect("open_read failed");

    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).expect("reading header");
    let header = DatabaseHeader::parse(&header_buf).expect("parsing header");

    let schema = decode_schema(file.as_ref(), &header);

    for entry in &schema {
        if entry.kind != "table" || entry.name.starts_with("sqlite_") {
            continue;
        }
        if is_virtual_table(&entry.sql) {
            eprintln!(
                "warning: skipping virtual table {} (graceful unknown)",
                entry.name
            );
            continue;
        }

        let defs = extract_column_defs(&entry.sql);
        let columns = column_names(&defs);
        let rowid_alias = rowid_alias_index(&defs);
        let mut rows = Vec::new();
        walk_table_btree(file.as_ref(), &header, entry.rootpage as u32, &mut rows);
        write_csv(
            path,
            &entry.name,
            &columns,
            rowid_alias,
            &rows,
            header.text_encoding,
        );
    }
}
