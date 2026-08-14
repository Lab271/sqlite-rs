// Spike #7 / #004 — de-risk WAL frame reading, the quirkiest unexercised
// corner of the Tier 0 READ CORE (see .openspec/specs/001-architecture).
// Throwaway, self-contained experiment (see findings.md); the b-tree walk
// and record decode below are copied from spike 002 (#4) rather than
// depended on, matching that spike's own precedent of a single, disposable
// crate — the only new piece is the WAL overlay (src/wal.rs).
mod wal;

use std::collections::HashMap;
use std::fmt;
use std::fs;

const HEADER_SIZE: usize = 100;

struct DbFile {
    bytes: Vec<u8>,
    page_size: usize,
    reserved_space: usize,
    wal_pages: HashMap<u32, Vec<u8>>,
    wal_db_size: u32,
}

impl DbFile {
    fn open(path: &str) -> Self {
        let bytes = fs::read(path).expect("read fixture db");
        assert_eq!(&bytes[0..16], b"SQLite format 3\0", "magic header mismatch");

        let page_size_raw = u16::from_be_bytes([bytes[16], bytes[17]]);
        let page_size = if page_size_raw == 1 { 65536 } else { page_size_raw as usize };
        let reserved_space = bytes[20] as usize;

        DbFile { bytes, page_size, reserved_space, wal_pages: HashMap::new(), wal_db_size: 0 }
    }

    fn load_wal(&mut self, path: &str) {
        let Ok(wal_bytes) = fs::read(path) else {
            println!("(no {path} — reading main db file only)");
            return;
        };
        println!("-- parsing {path} --");
        let header = wal::WalHeader::parse(&wal_bytes);
        println!(
            "wal header: page_size={} salts=({:#010x},{:#010x}) checksum_byte_order={}",
            header.page_size,
            header.salt1,
            header.salt2,
            if header.native_checksum { "native" } else { "big-endian" }
        );
        assert_eq!(header.page_size, self.page_size, "WAL page size disagrees with main db header");
        let (pages, db_size) = wal::committed_pages(&header, &wal_bytes);
        println!("wal committed pages: {:?} (db size after last commit: {db_size})", {
            let mut nums: Vec<_> = pages.keys().copied().collect();
            nums.sort();
            nums
        });
        self.wal_pages = pages;
        self.wal_db_size = db_size;
    }

    fn page_count(&self) -> u32 {
        let header_page_count = u32::from_be_bytes(self.bytes[28..32].try_into().unwrap());
        self.wal_db_size.max(header_page_count)
    }

    /// Page content, overridden by the WAL's committed version if present.
    fn read_page(&self, page_num: u32) -> Vec<u8> {
        if let Some(p) = self.wal_pages.get(&page_num) {
            return p.clone();
        }
        let off = (page_num as usize - 1) * self.page_size;
        self.bytes[off..off + self.page_size].to_vec()
    }

    /// Decode every row on the leaf table b-tree rooted at `page_num` as
    /// (rowid, record_bytes) pairs — copied from spike 002, adapted to
    /// read through `read_page` (page-relative offsets throughout, same as
    /// spike 002 found: cell pointers are relative to the page's own start
    /// even on page 1, never to where its b-tree header begins).
    fn read_table_leaf(&self, page_num: u32) -> Vec<(i64, Vec<u8>)> {
        let page = self.read_page(page_num);
        let header_start = if page_num == 1 { HEADER_SIZE } else { 0 };

        let page_type = page[header_start];
        assert_eq!(page_type, 0x0d, "expected a leaf table b-tree page (0x0d), got {page_type:#04x} — interior/index pages are out of scope for this spike");

        let num_cells = u16::from_be_bytes([page[header_start + 3], page[header_start + 4]]) as usize;
        let cell_ptr_array = header_start + 8;

        let mut rows = Vec::with_capacity(num_cells);
        for i in 0..num_cells {
            let ptr_offset = cell_ptr_array + i * 2;
            let cell_offset = u16::from_be_bytes([page[ptr_offset], page[ptr_offset + 1]]) as usize;

            let (payload_len, n1) = read_varint(&page[cell_offset..]);
            let (rowid, n2) = read_varint(&page[cell_offset + n1..]);
            let payload_start = cell_offset + n1 + n2;

            let max_local = (self.page_size - self.reserved_space - 35) as i64;
            assert!(
                payload_len <= max_local,
                "payload {payload_len} bytes would need an overflow page (>{max_local} local) — out of scope for this spike"
            );

            let payload = page[payload_start..payload_start + payload_len as usize].to_vec();
            rows.push((rowid, payload));
        }
        rows
    }
}

fn read_varint(buf: &[u8]) -> (i64, usize) {
    let mut result: i64 = 0;
    for (i, &byte) in buf.iter().enumerate().take(8) {
        result = (result << 7) | (byte & 0x7f) as i64;
        if byte & 0x80 == 0 {
            return (result, i + 1);
        }
    }
    result = (result << 8) | buf[8] as i64;
    (result, 9)
}

#[derive(Debug, Clone)]
enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "NULL"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Real(r) => write!(f, "{r}"),
            Value::Text(s) => write!(f, "{s}"),
            Value::Blob(b) => {
                write!(f, "X'")?;
                for byte in b {
                    write!(f, "{byte:02X}")?;
                }
                write!(f, "'")
            }
        }
    }
}

fn decode_record(payload: &[u8]) -> Vec<Value> {
    let (header_len, n) = read_varint(payload);
    let header_len = header_len as usize;

    let mut serial_types = Vec::new();
    let mut pos = n;
    while pos < header_len {
        let (serial_type, n) = read_varint(&payload[pos..]);
        serial_types.push(serial_type);
        pos += n;
    }

    let mut body_pos = header_len;
    let mut values = Vec::with_capacity(serial_types.len());
    for st in serial_types {
        let (value, len) = decode_serial_value(st, &payload[body_pos..]);
        values.push(value);
        body_pos += len;
    }
    values
}

fn decode_serial_value(serial_type: i64, body: &[u8]) -> (Value, usize) {
    match serial_type {
        0 => (Value::Null, 0),
        1 => (Value::Int(body[0] as i8 as i64), 1),
        2 => (Value::Int(i16::from_be_bytes([body[0], body[1]]) as i64), 2),
        4 => (Value::Int(i32::from_be_bytes(body[0..4].try_into().unwrap()) as i64), 4),
        6 => (Value::Int(i64::from_be_bytes(body[0..8].try_into().unwrap())), 8),
        7 => (Value::Real(f64::from_be_bytes(body[0..8].try_into().unwrap())), 8),
        8 => (Value::Int(0), 0),
        9 => (Value::Int(1), 0),
        n if n >= 12 && n % 2 == 0 => {
            let len = ((n - 12) / 2) as usize;
            (Value::Blob(body[0..len].to_vec()), len)
        }
        n => {
            let len = ((n - 13) / 2) as usize;
            let text = String::from_utf8_lossy(&body[0..len]).into_owned();
            (Value::Text(text), len)
        }
    }
}

fn dump(db_path: &str, wal_path: &str) {
    println!("\n=== {db_path} (+ {wal_path}) ===");
    let mut db = DbFile::open(db_path);
    db.load_wal(wal_path);
    println!("page_count (post-WAL-merge): {}", db.page_count());

    let master_rows = db.read_table_leaf(1);
    let mut t_rootpage = None;
    for (_rowid, payload) in &master_rows {
        let cols = decode_record(payload);
        if let Value::Text(name) = &cols[1] {
            if name == "t" {
                if let Value::Int(rp) = cols[3] {
                    t_rootpage = Some(rp as u32);
                }
            }
        }
    }

    let t_rootpage = t_rootpage.expect("table 't' not found in sqlite_master");
    let t_rows = db.read_table_leaf(t_rootpage);
    println!("table t (page {t_rootpage}), {} row(s):", t_rows.len());
    for (rowid, payload) in &t_rows {
        let cols = decode_record(payload);
        println!("  rowid={rowid} a={} b={}", cols[0], cols[1]);
    }
}

fn main() {
    dump("fixture.db", "fixture.db-wal");
    dump("fixture_bigendian.db", "fixture_bigendian.db-wal");
    dump("fixture_trailing.db", "fixture_trailing.db-wal");
    dump("fixture_stale.db", "fixture_stale.db-wal");
}
