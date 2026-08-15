//! Minimal schema reader (Tier 0): decodes `sqlite_master` into enough
//! structure to drive the table/index b-tree cursors, without depending
//! on the (future) full SQL parser. See `.openspec/specs/002-parser/spec.md`
//! Requirement 5 — this module lives outside `src/parser/` by design.

mod ddl_reader;

pub use ddl_reader::{read_schema, DdlError, TableSchema};
