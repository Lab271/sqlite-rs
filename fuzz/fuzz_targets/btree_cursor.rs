#![no_main]

use libfuzzer_sys::fuzz_target;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::record::TextEncoding;
use sqlite_rs::vfs::{PageError, PageSource};

/// Serves the same fuzz-supplied bytes (padded/truncated to `page_size`)
/// for a small, fixed set of page numbers — enough for the fuzzer to
/// discover interior/leaf/overflow structures and self-referencing
/// cycles, bounded so exploration stays cheap.
struct FuzzPageSource<'a> {
    data: &'a [u8],
    page_size: u32,
}

impl PageSource for FuzzPageSource<'_> {
    fn read_page(&self, page_num: u32) -> Result<Vec<u8>, PageError> {
        if page_num == 0 || page_num > 8 {
            return Err(PageError::InvalidPageNumber);
        }
        let mut buf = vec![0u8; self.page_size as usize];
        let n = self.data.len().min(buf.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        Ok(buf)
    }
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    let header = DatabaseHeader {
        page_size: 512, // minimum valid page size
        write_version: 1,
        read_version: 1,
        reserved_space: 0,
        page_count: 8,
        freelist_trunk_page: 0,
        freelist_page_count: 0,
        schema_cookie: 0,
        schema_format: 0,
        largest_root_btree_page: 0,
        text_encoding: TextEncoding::Utf8,
        user_version: 0,
        application_id: 0,
    };
    let source = FuzzPageSource {
        data,
        page_size: header.page_size,
    };
    let mut cursor = TableCursor::new(source, &header, 2);

    let _ = cursor.first();
    while let Ok(Some(_)) = cursor.next() {}

    let _ = cursor.seek(1);
});
