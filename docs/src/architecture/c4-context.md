# System Context

```mermaid
C4Context
    title sqlite-rs System Context

    Person(dev, "Developer", "Uses sqlite-rs API or CLI")
    System(sqliteRs, "sqlite-rs", "Pure-Rust SQLite implementation")
    System_Ext(dbFile, "SQLite Database", ".db/.sqlite file")
    System_Ext(sqliteCli, "sqlite3 CLI", "Reference implementation")

    Rel(dev, sqliteRs, "Queries")
    Rel(sqliteRs, dbFile, "Read/Write")
    BiRel(sqliteRs, sqliteCli, "Parity testing")
```

`sqlite-rs` reads and writes the same on-disk file format as the reference
`sqlite3` implementation, and is validated against it via oracle-diff parity
tests (see `.openspec/specs/004-corpus`).
