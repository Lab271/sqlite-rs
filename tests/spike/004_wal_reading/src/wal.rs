// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
// WAL frame reading — issue #7. Byte layout derived from SQLite's own
// fileformat2.html (not documented anywhere in this repo's specs at the
// time of writing; see findings.md).
//
// WAL header (32 bytes):
//   0..4   magic: 0x377f0682 (checksums big-endian) or 0x377f0683 (native)
//   4..8   file format version (3007000)
//   8..12  page size
//   12..16 checkpoint sequence number
//   16..20 salt-1
//   20..24 salt-2
//   24..28 checksum-1 (of bytes 0..24)
//   28..32 checksum-2
//
// Frame header (24 bytes), immediately followed by `page_size` bytes of
// page content:
//   0..4   page number
//   4..8   size of the database in pages, AFTER this frame, if this frame
//          committed a transaction — 0 if this frame did not commit
//   8..12  salt-1 (copied from the WAL header)
//   12..16 salt-2 (copied from the WAL header)
//   16..20 checksum-1 (running, continues from the previous frame's, or
//          the header's if this is the first frame)
//   20..24 checksum-2

pub struct WalHeader {
    pub native_checksum: bool,
    pub page_size: usize,
    pub salt1: u32,
    pub salt2: u32,
    pub header_checksum: (u32, u32),
}

impl WalHeader {
    pub fn parse(bytes: &[u8]) -> Self {
        assert!(bytes.len() >= 32, "WAL file shorter than the 32-byte header");
        let magic = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        // Empirically verified against real sqlite3-produced WAL files
        // (see findings.md) — the reverse of what the magic byte's name
        // suggests at first glance: 0x82 is the ORIGINAL/default mode,
        // which checksums in the writer's own native byte order (cheaper,
        // no byte-swapping on every write); 0x83 is the newer, portable
        // mode that always uses big-endian regardless of host.
        let native_checksum = match magic {
            0x377f0682 => true,
            0x377f0683 => false,
            _ => panic!("bad WAL magic {magic:#010x}"),
        };
        let page_size = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let salt1 = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let salt2 = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        let stored = (
            u32::from_be_bytes(bytes[24..28].try_into().unwrap()),
            u32::from_be_bytes(bytes[28..32].try_into().unwrap()),
        );
        let computed = checksum(native_checksum, &bytes[0..24], (0, 0));
        assert_eq!(computed, stored, "WAL header checksum mismatch — corrupt WAL");
        WalHeader { native_checksum, page_size, salt1, salt2, header_checksum: stored }
    }
}

/// SQLite's WAL checksum: `data` must be a multiple of 8 bytes, read as
/// pairs of 32-bit words — in the host's native byte order if
/// `native_checksum`, big-endian otherwise (this is exactly what the WAL
/// magic number's low byte selects; see the comment on `native_checksum`
/// above for which way round that is). Continues a running (s1, s2) pair;
/// feed in (0, 0) to start.
pub fn checksum(native_checksum: bool, data: &[u8], init: (u32, u32)) -> (u32, u32) {
    assert_eq!(data.len() % 8, 0, "checksum input must be a multiple of 8 bytes");
    let (mut s1, mut s2) = init;
    for chunk in data.chunks_exact(8) {
        let w0 = read_word(native_checksum, &chunk[0..4]);
        let w1 = read_word(native_checksum, &chunk[4..8]);
        s1 = s1.wrapping_add(w0).wrapping_add(s2);
        s2 = s2.wrapping_add(w1).wrapping_add(s1);
    }
    (s1, s2)
}

fn read_word(native_checksum: bool, b: &[u8]) -> u32 {
    let arr: [u8; 4] = b.try_into().unwrap();
    if native_checksum {
        u32::from_ne_bytes(arr)
    } else {
        u32::from_be_bytes(arr)
    }
}

/// Walks every frame in `wal_bytes` (past the 32-byte header) and returns
/// the page map as of the LAST committed transaction, plus that commit's
/// declared database size in pages.
///
/// A page's mapping only gets published into the returned map when a
/// commit frame (db-size-if-commit != 0) is reached — frames from a
/// transaction that never committed (rolled back, or the process died
/// mid-write) update a scratch `candidate` map but are never published, so
/// they naturally fall away. Scanning stops the instant a frame's salts
/// don't match the WAL header's (a leftover/foreign frame from a
/// different WAL generation) or its checksum doesn't verify (corrupt or
/// incomplete tail) — either way, whatever was last published survives.
pub fn committed_pages(header: &WalHeader, wal_bytes: &[u8]) -> (std::collections::HashMap<u32, Vec<u8>>, u32) {
    let frame_size = 24 + header.page_size;
    let mut offset = 32;
    let mut running = header.header_checksum;
    let mut candidate: std::collections::HashMap<u32, Vec<u8>> = std::collections::HashMap::new();
    let mut committed: std::collections::HashMap<u32, Vec<u8>> = std::collections::HashMap::new();
    let mut committed_db_size = 0u32;

    while offset + frame_size <= wal_bytes.len() {
        let fh = &wal_bytes[offset..offset + 24];
        let page_number = u32::from_be_bytes(fh[0..4].try_into().unwrap());
        let db_size = u32::from_be_bytes(fh[4..8].try_into().unwrap());
        let salt1 = u32::from_be_bytes(fh[8..12].try_into().unwrap());
        let salt2 = u32::from_be_bytes(fh[12..16].try_into().unwrap());
        let stored_checksum = (
            u32::from_be_bytes(fh[16..20].try_into().unwrap()),
            u32::from_be_bytes(fh[20..24].try_into().unwrap()),
        );

        if salt1 != header.salt1 || salt2 != header.salt2 {
            println!(
                "  frame at offset {offset}: salt mismatch ({salt1:#010x}/{salt2:#010x} vs header {:#010x}/{:#010x}) — stopping scan, treating rest of file as foreign/unwritten",
                header.salt1, header.salt2
            );
            break;
        }

        let page_content = &wal_bytes[offset + 24..offset + 24 + header.page_size];
        let after_frame_header = checksum(header.native_checksum, &fh[0..8], running);
        let after_page = checksum(header.native_checksum, page_content, after_frame_header);

        if after_page != stored_checksum {
            println!("  frame at offset {offset}: checksum mismatch — stopping scan (corrupt or incomplete tail)");
            break;
        }
        running = after_page;

        candidate.insert(page_number, page_content.to_vec());

        if db_size != 0 {
            committed = candidate.clone();
            committed_db_size = db_size;
            println!("  frame at offset {offset}: page {page_number}, COMMIT (db now {db_size} pages)");
        } else {
            println!("  frame at offset {offset}: page {page_number}, not a commit frame");
        }

        offset += frame_size;
    }

    (committed, committed_db_size)
}
