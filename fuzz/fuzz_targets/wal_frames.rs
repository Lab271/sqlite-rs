#![no_main]

use libfuzzer_sys::fuzz_target;

use sqlite_rs::pager::wal::{committed_pages, WalHeader};

fuzz_target!(|data: &[u8]| {
    if let Ok(header) = WalHeader::parse(data) {
        let _ = committed_pages(&header, data);
    }
});
