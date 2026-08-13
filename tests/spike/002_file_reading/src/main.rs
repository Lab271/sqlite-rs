// Spike #4 / #002 — de-risk Tier 0 READ CORE: walk a real SQLite file from raw
// bytes to typed row values. Throwaway, single-file experiment (see
// tests/spike/002_file_reading/findings.md for the write-up). Deliberately
// scoped to the fixture's shape: single-page table b-trees, no overflow
// chains, no multi-page (interior) b-trees — those are noted as unexplored
// in the findings, not silently faked.

use std::fmt;
use std::fs;

const HEADER_SIZE: usize = 100;

struct DbFile {
    bytes: Vec<u8>,
    page_size: usize,
    reserved_space: usize,
}

impl DbFile {
    fn open(path: &str) -> Self {
        let bytes = fs::read(path).expect("read fixture.db");
        assert_eq!(&bytes[0..16], b"SQLite format 3\0", "magic header mismatch");

        let page_size_raw = u16::from_be_bytes([bytes[16], bytes[17]]);
        let page_size = if page_size_raw == 1 { 65536 } else { page_size_raw as usize };

        let reserved_space = bytes[20] as usize;
        let page_count = u32::from_be_bytes(bytes[28..32].try_into().unwrap());
        let text_encoding = u32::from_be_bytes(bytes[56..60].try_into().unwrap());

        println!(
            "header: page_size={page_size} reserved_space={reserved_space} page_count={page_count} text_encoding={text_encoding} (1=utf8,2=utf16le,3=utf16be)"
        );

        DbFile { bytes, page_size, reserved_space }
    }

    /// Byte offset (into `self.bytes`) where page `page_num` (1-indexed) starts.
    fn page_offset(&self, page_num: u32) -> usize {
        (page_num as usize - 1) * self.page_size
    }

    /// Decode every row on the leaf table b-tree rooted at `page_num` as
    /// (rowid, record_bytes) pairs. Assumes a single leaf page (0x0d) — no
    /// interior pages, no overflow. Panics loudly if that assumption breaks,
    /// since a silent wrong-answer is worse than a crash in a spike.
    fn read_table_leaf(&self, page_num: u32) -> Vec<(i64, &[u8])> {
        let page_start = self.page_offset(page_num);
        // Page 1 carries the 100-byte file header before its b-tree page
        // header; every other page's b-tree header starts at the page's
        // first byte. Cell pointers, however, are always relative to
        // page_start (byte 0 of the page), header or no header.
        let header_start = page_start + if page_num == 1 { HEADER_SIZE } else { 0 };

        let page_type = self.bytes[header_start];
        assert_eq!(page_type, 0x0d, "expected a leaf table b-tree page (0x0d), got {page_type:#04x} — interior/index pages are out of scope for this spike");

        let num_cells = u16::from_be_bytes([self.bytes[header_start + 3], self.bytes[header_start + 4]]) as usize;
        let cell_ptr_array = header_start + 8;

        let mut rows = Vec::with_capacity(num_cells);
        for i in 0..num_cells {
            let ptr_offset = cell_ptr_array + i * 2;
            let cell_offset =
                page_start + u16::from_be_bytes([self.bytes[ptr_offset], self.bytes[ptr_offset + 1]]) as usize;

            let (payload_len, n1) = read_varint(&self.bytes[cell_offset..]);
            let (rowid, n2) = read_varint(&self.bytes[cell_offset + n1..]);
            let payload_start = cell_offset + n1 + n2;

            // Usable size minus a 35-byte reserve is the max payload a table
            // leaf cell stores locally before spilling to an overflow page.
            let max_local = (self.page_size - self.reserved_space - 35) as i64;
            assert!(
                payload_len <= max_local,
                "payload {payload_len} bytes would need an overflow page (>{max_local} local) — out of scope for this spike"
            );

            let payload = &self.bytes[payload_start..payload_start + payload_len as usize];
            rows.push((rowid, payload));
        }
        rows
    }
}

/// SQLite varint: big-endian, 7 bits per byte with a continuation bit, up to
/// 9 bytes (the 9th contributes a full 8 bits). Returns (value, bytes_read).
fn read_varint(buf: &[u8]) -> (i64, usize) {
    let mut result: i64 = 0;
    for (i, &byte) in buf.iter().enumerate().take(8) {
        result = (result << 7) | (byte & 0x7f) as i64;
        if byte & 0x80 == 0 {
            return (result, i + 1);
        }
    }
    // 9th byte contributes all 8 bits, no continuation bit.
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
            Value::Real(r) => {
                // Rust's default {} Display spells huge/tiny floats out in
                // full decimal (e.g. a 300-digit string for 2.5e300) rather
                // than switching to scientific notation the way sqlite3's
                // quote() does — this is a display-only difference (the
                // decoded bits are identical either way), not a decoding bug.
                if *r != 0.0 && (r.abs() >= 1e16 || r.abs() < 1e-4) {
                    write!(f, "{r:e}")
                } else {
                    write!(f, "{r}")
                }
            }
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

/// Decode a record (the payload of a table b-tree cell) into column values,
/// per the record-format doc: varint header length, then one varint serial
/// type per column, then the column bodies back-to-back.
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
        3 => {
            let b = [body[0], body[1], body[2]];
            let mut v = ((b[0] as i64) << 16) | ((b[1] as i64) << 8) | (b[2] as i64);
            if b[0] & 0x80 != 0 {
                v -= 1 << 24; // sign-extend 24-bit
            }
            (Value::Int(v), 3)
        }
        4 => (Value::Int(i32::from_be_bytes(body[0..4].try_into().unwrap()) as i64), 4),
        5 => {
            let b = &body[0..6];
            let mut v: i64 = 0;
            for &byte in b {
                v = (v << 8) | byte as i64;
            }
            if b[0] & 0x80 != 0 {
                v -= 1 << 48; // sign-extend 48-bit
            }
            (Value::Int(v), 6)
        }
        6 => (Value::Int(i64::from_be_bytes(body[0..8].try_into().unwrap())), 8),
        7 => (Value::Real(f64::from_be_bytes(body[0..8].try_into().unwrap())), 8),
        8 => (Value::Int(0), 0),
        9 => (Value::Int(1), 0),
        10 | 11 => panic!("serial type {serial_type} is reserved/internal — not expected in a user record"),
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

fn main() {
    let db = DbFile::open("fixture.db");

    println!("\n== sqlite_master (page 1) ==");
    let master_rows = db.read_table_leaf(1);
    let mut t_rootpage = None;
    for (rowid, payload) in &master_rows {
        let cols = decode_record(payload);
        println!(
            "rowid={rowid} type={} name={} tbl_name={} rootpage={} sql={}",
            cols[0], cols[1], cols[2], cols[3], cols[4]
        );
        if let Value::Text(name) = &cols[1] {
            if name == "t" {
                if let Value::Int(rp) = cols[3] {
                    t_rootpage = Some(rp as u32);
                }
            }
        }
    }

    let t_rootpage = t_rootpage.expect("table 't' not found in sqlite_master");
    println!("\n== table t (page {t_rootpage}) ==");
    let t_rows = db.read_table_leaf(t_rootpage);
    for (rowid, payload) in &t_rows {
        let cols = decode_record(payload);
        println!(
            "rowid={rowid} a={} b={} c={} d={} e={}",
            cols[0], cols[1], cols[2], cols[3], cols[4]
        );
    }
}
