//! DDL codegen: `CREATE TABLE`/`DROP TABLE`/`CREATE INDEX`/`DROP INDEX`
//! (#215). Each compiles to a single procedural opcode at exec time —
//! no per-row cursor work, unlike the DML statements in the parent
//! `codegen` module. See each submodule's doc for its exact opcode
//! shape.

mod create_index;
mod create_table;
mod drop_index;
mod drop_table;

pub use create_index::compile_create_index;
pub use create_table::compile_create_table;
pub use drop_index::compile_drop_index;
pub use drop_table::compile_drop_table;
