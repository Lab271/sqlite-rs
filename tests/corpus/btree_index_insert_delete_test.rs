//! #171 acceptance: index b-tree insert/delete (same split/merge
//! mechanics as the table write path, #168/#169, but index cell format
//! and key-comparison ordering) must produce files stock `sqlite3` opens,
//! `PRAGMA integrity_check`s cleanly, and reads back identically via
//! `select ... order by`. Follows `btree_insert_test.rs`'s pattern: seed
//! the fixture by shelling out to the oracle directly, then write through
//! `sqlite_rs::btree::{insert_entry, delete_entry}` and verify via the
//! oracle.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::{delete_entry, delete_row, insert_entry, insert_row};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::record::encode_record;
use sqlite_rs::record::{TextEncoding, Value};
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-btree-index-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn seed(oracle: &PathBuf, db: &PathBuf, sql: &str) {
    let status = Command::new(oracle).arg(db).arg(sql).status().unwrap();
    assert!(status.success());
}

fn page_size_of(vfs: &UnixVfs, db: &Path) -> u32 {
    let pager = Pager::open(vfs, db, 4096).unwrap();
    let header = pager.read_page(1).unwrap();
    let page_size = u16::from_be_bytes([header[16], header[17]]) as u32;
    if page_size == 1 {
        65536
    } else {
        page_size
    }
}

fn read_header(vfs: &UnixVfs, db: &Path, page_size: u32) -> DatabaseHeader {
    let pager = Pager::open(vfs, db, page_size).unwrap();
    let raw = pager.read_page(1).unwrap();
    let mut buf = [0u8; 100];
    buf.copy_from_slice(&raw[..100]);
    DatabaseHeader::parse(&buf).unwrap()
}

fn oracle_select(oracle: &PathBuf, db: &PathBuf, sql: &str) -> String {
    let out = Command::new(oracle)
        .arg("-readonly")
        .arg("-list")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn root_page_of_name(oracle: &PathBuf, db: &PathBuf, name: &str) -> u32 {
    oracle_select(
        oracle,
        db,
        &format!("select rootpage from sqlite_master where name = '{name}';"),
    )
    .parse()
    .unwrap()
}

/// Composite key `(b, rowid)`: the indexed column plus the referenced
/// table's rowid — the same shape the codegen/VDBE write path would
/// build for an ordinary secondary index entry.
fn secondary_key(b: &str, rowid: i64) -> Vec<Value> {
    vec![Value::Text(b.to_string().into()), Value::Integer(rowid)]
}

#[test]
fn insert_single_entry_into_a_secondary_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("insert_single_entry_into_a_secondary_index");
        return;
    };

    let db = scratch_db("single_entry");
    seed(
        &oracle,
        &db,
        "create table t(a integer primary key, b text); create index idx_b on t(b);",
    );

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let table_root = root_page_of_name(&oracle, &db, "t");
    let index_root = root_page_of_name(&oracle, &db, "idx_b");

    // Insert through our own table AND index write paths together (#168
    // table insert + #171 index insert) — PRAGMA integrity_check
    // cross-validates the index's row count against the table's, so
    // both must stay in lockstep, exactly as the eventual VDBE INSERT
    // opcode will maintain them.
    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        for (rowid, b) in [(1i64, "existing"), (2, "new row")] {
            let payload = encode_record(
                &[Value::Null, Value::Text(b.to_string().into())],
                TextEncoding::Utf8,
            );
            insert_row(&mut pager, &header, table_root, rowid, &payload).unwrap();
            insert_entry(
                &mut pager,
                &header,
                index_root,
                &secondary_key(b, rowid),
                TextEncoding::Utf8,
            )
            .unwrap();
        }
        pager.flush().unwrap();
    }

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(
        oracle_select(&oracle, &db, "select a, b from t order by a;"),
        "1|existing\n2|new row"
    );

    let pager_ro = Pager::open(&vfs, &db, page_size).unwrap();
    let mut cursor =
        sqlite_rs::btree::IndexCursor::new(pager_ro, header.usable_page_size(), index_root);
    let mut rows = Vec::new();
    let mut row = cursor.first().unwrap();
    while let Some(r) = row {
        rows.push(r);
        row = cursor.next().unwrap();
    }
    assert_eq!(rows.len(), 2);
    let decoded: Vec<Vec<Value>> = rows
        .iter()
        .map(|r| sqlite_rs::record::decode_record(&r.payload, TextEncoding::Utf8).unwrap())
        .collect();
    assert_eq!(decoded[0], secondary_key("existing", 1));
    assert_eq!(decoded[1], secondary_key("new row", 2));

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn bulk_insert_forces_index_splits_and_reads_back_in_order() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("bulk_insert_forces_index_splits_and_reads_back_in_order");
        return;
    };

    let db = scratch_db("bulk_split");
    seed(
        &oracle,
        &db,
        "create table t(a integer primary key, b text); create index idx_b on t(b);",
    );

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let table_root = root_page_of_name(&oracle, &db, "t");
    let index_root = root_page_of_name(&oracle, &db, "idx_b");

    // ~200-byte keys: enough entries to force several index leaf splits
    // (and, per index_insert.rs's promote-and-remove shape, cascading
    // interior splits too). Table and index inserted together so
    // integrity_check's cross-count validation passes.
    let filler = "x".repeat(190);
    let n = 500i64;

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        for i in 1..=n {
            let b = format!("{filler}-{i:04}");
            let payload = encode_record(
                &[Value::Null, Value::Text(b.clone().into())],
                TextEncoding::Utf8,
            );
            insert_row(&mut pager, &header, table_root, i, &payload).unwrap();
            insert_entry(
                &mut pager,
                &header,
                index_root,
                &secondary_key(&b, i),
                TextEncoding::Utf8,
            )
            .unwrap();
        }
        pager.flush().unwrap();
    }

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(
        oracle_select(&oracle, &db, "select count(*) from t;"),
        n.to_string()
    );

    // Read every entry back via our own read-side IndexCursor and confirm
    // ascending BINARY-collation order matches how many we inserted.
    let source_vfs = UnixVfs;
    let pager_ro = Pager::open(&source_vfs, &db, page_size).unwrap();
    let mut cursor =
        sqlite_rs::btree::IndexCursor::new(pager_ro, header.usable_page_size(), index_root);
    let mut rows = Vec::new();
    let mut row = cursor.first().unwrap();
    while let Some(r) = row {
        rows.push(r);
        row = cursor.next().unwrap();
    }
    assert_eq!(rows.len() as i64, n);
    for i in 1..rows.len() {
        let prev =
            sqlite_rs::record::decode_record(&rows[i - 1].payload, TextEncoding::Utf8).unwrap();
        let cur = sqlite_rs::record::decode_record(&rows[i].payload, TextEncoding::Utf8).unwrap();
        let prev_b = match &prev[0] {
            Value::Text(s) => s.clone(),
            _ => panic!("expected text"),
        };
        let cur_b = match &cur[0] {
            Value::Text(s) => s.clone(),
            _ => panic!("expected text"),
        };
        assert!(prev_b <= cur_b, "entries out of order: {prev_b} > {cur_b}");
    }

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn delete_all_entries_leaves_an_empty_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("delete_all_entries_leaves_an_empty_index");
        return;
    };

    let db = scratch_db("delete_all");
    seed(
        &oracle,
        &db,
        "create table t(a integer primary key, b text); create index idx_b on t(b);",
    );

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let table_root = root_page_of_name(&oracle, &db, "t");
    let index_root = root_page_of_name(&oracle, &db, "idx_b");

    let filler = "x".repeat(190);
    let n = 200i64;
    let rows: Vec<(i64, String)> = (1..=n).map(|i| (i, format!("{filler}-{i:04}"))).collect();

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        for (rowid, b) in &rows {
            let payload = encode_record(
                &[Value::Null, Value::Text(b.clone().into())],
                TextEncoding::Utf8,
            );
            insert_row(&mut pager, &header, table_root, *rowid, &payload).unwrap();
            insert_entry(
                &mut pager,
                &header,
                index_root,
                &secondary_key(b, *rowid),
                TextEncoding::Utf8,
            )
            .unwrap();
        }
        for (rowid, b) in &rows {
            delete_entry(
                &mut pager,
                &header,
                index_root,
                &secondary_key(b, *rowid),
                TextEncoding::Utf8,
            )
            .unwrap();
            delete_row(&mut pager, &header, table_root, *rowid).unwrap();
        }
        pager.flush().unwrap();
    }

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "select count(*) from t;"), "0");

    let source_vfs = UnixVfs;
    let pager_ro = Pager::open(&source_vfs, &db, page_size).unwrap();
    let mut cursor =
        sqlite_rs::btree::IndexCursor::new(pager_ro, header.usable_page_size(), index_root);
    assert!(cursor.first().unwrap().is_none());

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn without_rowid_table_insert_and_delete_round_trip() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("without_rowid_table_insert_and_delete_round_trip");
        return;
    };

    let db = scratch_db("without_rowid");
    seed(
        &oracle,
        &db,
        "create table t(k text primary key, v text) without rowid;",
    );

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of_name(&oracle, &db, "t");

    let row = |k: &str, v: &str| {
        vec![
            Value::Text(k.to_string().into()),
            Value::Text(v.to_string().into()),
        ]
    };

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_entry(
            &mut pager,
            &header,
            root,
            &row("key1", "value one"),
            TextEncoding::Utf8,
        )
        .unwrap();
        insert_entry(
            &mut pager,
            &header,
            root,
            &row("key2", "value two"),
            TextEncoding::Utf8,
        )
        .unwrap();
        insert_entry(
            &mut pager,
            &header,
            root,
            &row("key3", "value three"),
            TextEncoding::Utf8,
        )
        .unwrap();
        delete_entry(
            &mut pager,
            &header,
            root,
            &row("key2", "value two"),
            TextEncoding::Utf8,
        )
        .unwrap();
        pager.flush().unwrap();
    }

    assert_integrity_check_ok(&oracle, &db);

    let source_vfs = UnixVfs;
    let pager_ro = Pager::open(&source_vfs, &db, page_size).unwrap();
    let mut cursor = sqlite_rs::btree::IndexCursor::new(pager_ro, header.usable_page_size(), root);
    let mut rows = Vec::new();
    let mut r = cursor.first().unwrap();
    while let Some(row) = r {
        rows.push(row);
        r = cursor.next().unwrap();
    }
    assert_eq!(rows.len(), 2);
    let decoded: Vec<Vec<Value>> = rows
        .iter()
        .map(|r| sqlite_rs::record::decode_record(&r.payload, TextEncoding::Utf8).unwrap())
        .collect();
    assert_eq!(decoded[0], row("key1", "value one"));
    assert_eq!(decoded[1], row("key3", "value three"));

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn duplicate_key_insert_is_rejected_against_a_real_fixture() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("duplicate_key_insert_is_rejected_against_a_real_fixture");
        return;
    };

    let db = scratch_db("dup_key");
    seed(
        &oracle,
        &db,
        "create table t(a integer primary key, b text); create index idx_b on t(b); \
         insert into t values (1, 'x');",
    );

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of_name(&oracle, &db, "idx_b");

    let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
    let key = secondary_key("x", 1);
    let err = insert_entry(&mut pager, &header, root, &key, TextEncoding::Utf8).unwrap_err();
    assert!(matches!(err, sqlite_rs::btree::BtreeError::DuplicateKey));

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}
