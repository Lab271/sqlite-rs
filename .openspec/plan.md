# Development Plan

Suggested order for rebuilding SQLite in Rust.

## Build Order

| Phase | Layer | Why this order |
|-------|-------|----------------|
| **1** | **Tokenizer** | No dependencies, easy to test against SQLite |
| **2** | **Parser** | Depends only on tokenizer, lemon-rs has grammar ready |
| **3** | **AST** | Define types, wire to parser actions |
| **4** | **VFS** | Platform abstraction — needed before storage layers |
| **5** | **Pager** | Page cache + journal — can test with mock VFS |
| **6** | **B-Tree** | Depends on pager, can test with real file format |
| **7** | **VDBE** | Bytecode VM — can test with mock B-tree first |
| **8** | **Code Generator** | Last — compiles AST to bytecode, needs all below it |
| **9** | **SQL Interface** | Public API wrapping everything |

## Rationale

```
Parse-first:     Tokenizer → Parser → AST
                         ↓
Storage-up:      VFS → Pager → B-Tree
                         ↓
Connect:         VDBE (uses B-Tree cursors)
                         ↓
Compile:         Code Generator (AST → VDBE bytecode)
                         ↓
Wrap:            SQL Interface
```

## Parallel Tracks

Two independent tracks until VDBE:

| Track | Work | Demo |
|-------|------|------|
| **Frontend** | Tokenizer → Parser → AST | Parse SQL, print AST |
| **Storage** | VFS → Pager → B-Tree | Read existing .sqlite files |

They merge at VDBE — the VM needs cursors (B-Tree) and executes programs compiled from AST.

## Milestones

| Milestone | Demo | Tracks |
|-----------|------|--------|
| **M1** | Parse SQL, print AST | Frontend |
| **M2** | Read existing SQLite file, dump schema | Storage |
| **M3** | Execute `SELECT * FROM sqlite_master` | VDBE + both |
| **M4** | Execute arbitrary SELECT on existing DB | Code Generator |
| **M5** | INSERT/UPDATE/DELETE (write path) | Full write path |
| **M6** | CREATE TABLE (DDL) | Schema modification |
| **M7** | WAL mode, crash recovery | Durability |

## First Month

Start M1 + M2 in parallel:

### M1: Frontend Track

1. Implement tokenizer (`src/parser/tokenizer.rs`)
2. Set up lemon-rs with SQLite grammar
3. Define AST types (`src/parser/ast.rs`)
4. Wire parser to AST builder
5. Test: parse → print → compare with SQLite

### M2: Storage Track

1. Define VFS trait (`src/vfs/mod.rs`)
2. Implement Unix VFS (`src/vfs/unix.rs`)
3. Implement pager read path (`src/pager/mod.rs`)
4. Implement B-tree read path (`src/btree/mod.rs`)
5. Test: open existing .sqlite, read `sqlite_master`

## Test Strategy

Use SQLite as the oracle throughout:

```bash
# Parse parity
sqlite3 :memory: ".read test.sql" 2>&1  # SQLite parse
sqlite-rs parse test.sql                 # Our parse
diff <(sqlite3 ...) <(sqlite-rs ...)     # Must match

# Query parity
sqlite3 test.db "SELECT * FROM t"        # SQLite result
sqlite-rs query test.db "SELECT * FROM t" # Our result
diff ...                                  # Must match
```

SQLite's test suite (700:1 ratio) is the ultimate oracle. Run against it continuously.

## Dependencies

```toml
[dependencies]
lemon-rs = "0.x"        # Parser generator (or lalrpop)
thiserror = "1"         # Error handling
memmap2 = "0.x"         # Memory-mapped I/O for VFS
parking_lot = "0.x"     # Faster mutexes for pager

[dev-dependencies]
rusqlite = "0.x"        # Oracle for testing
tempfile = "3"          # Test fixtures
```

## Risk Areas

| Area | Risk | Mitigation |
|------|------|------------|
| **B-Tree** | Complex, many edge cases | Test every operation against SQLite |
| **Pager/WAL** | Crash recovery is subtle | Use SQLite's crash tests |
| **Code Generator** | Largest module (~35K lines) | Implement incrementally, one statement type at a time |
| **File format** | Must be byte-compatible | Hex-diff against SQLite output |
