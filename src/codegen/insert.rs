//! `Insert` AST -> `Program` compilation (#195): builds on #194's write
//! opcodes (`NewRowid`/`MakeRecord`/`Insert`/`Delete`) plus constraint
//! checks (NOT NULL, PRIMARY KEY/rowid, CHECK, DEFAULT). Table
//! constraints aren't cached anywhere (`TableSchema` is deliberately
//! naive — see `src/schema/ddl_reader.rs`), so this module re-parses
//! `schema.sql` with the real parser to recover them, the same trick
//! `rowid_alias_column` already uses for the PK-rowid-alias fact.
//!
//! Secondary indexes are maintained on every row (#196): each index on
//! the table gets its own write cursor, and once a row is inserted the
//! table cursor is `SeekRowid`'d back onto it so the index key can be
//! read back via `Column`/`Rowid` and written via `IdxInsert` (see
//! `index_maintenance`).
//!
//! `InsertSource::Select` (#208) drives `select.rs`'s
//! `compile_select_scan` — the same scan/filter/project/ORDER BY/
//! DISTINCT/LIMIT machinery `compile_select` uses — with a `sink` that
//! feeds each projected row's registers into [`compile_row`] instead of
//! emitting `ResultRow`. `compile_row` itself is source-agnostic: its
//! per-column value comes from a `column_at(pos)` closure that either
//! hands back a literal-or-computed `Expr` (`VALUES`/`DEFAULT VALUES`)
//! or a [`ColumnSource::Reg`] already populated by the SELECT scan,
//! since every constraint check downstream of "the value is in
//! register `r`" (NOT NULL, CHECK, `MakeRecord`/`Insert`, index
//! maintenance) doesn't care how `r` was populated. The SELECT's
//! source-table scan gets its own cursor numbers (via
//! `select::ScanCursors`), offset above this module's target-table/
//! index cursors, so the two scans never collide within the same
//! program.
//!
//! UNIQUE constraints on non-rowid columns (#207) are enforced against
//! every index in `schema.indexes` with `unique: true`: `emit_unique_check`
//! probes the candidate row via the new `Opcode::NoConflict` real-index
//! seek+branch primitive (`src/vdbe/cursor.rs`, built on
//! `IndexCursor::seek`) and dispatches `ON CONFLICT` the same way
//! `emit_pk_conflict` does for the rowid-PK case. A composite
//! `PRIMARY KEY(...)`/`UNIQUE(...)` *table* constraint with no backing
//! `CREATE INDEX`/on-disk index (this codebase doesn't auto-create
//! `sqlite_autoindex_*` entries yet) has no real index to seek against,
//! so it still isn't enforced — that's a `CREATE TABLE`-side gap, not
//! an INSERT-codegen one.
//!
//! Known simplifications (deferred to follow-up tickets, not chased
//! here):
//! - `InsertSource::Select` does not compile (`VALUES`/`DEFAULT VALUES`
//!   only) — filed separately from #195.
//! - An index with a `DESC` column is rejected outright
//!   (`CodegenError::Unsupported`), not silently mis-keyed — see
//!   `index_maintenance`.
//! - `WITHOUT ROWID` tables are rejected (`Unsupported`) — the
//!   rowid-based insert/seek machinery this module uses doesn't apply.
//! - `ON CONFLICT ROLLBACK`/`FAIL` both compile identically to `ABORT`
//!   (a single `Halt`) — there's no per-transaction/per-statement
//!   partial-rollback machinery at the VDBE layer to distinguish them.
//! - A `CHECK` expression that references the rowid-alias
//!   (`INTEGER PRIMARY KEY`) column sees `NULL` for it, not the row's
//!   actual (about-to-be-assigned) rowid — the on-disk record always
//!   stores `NULL` there (matching stock SQLite), and the pseudo-cursor
//!   `CHECK` evaluates against is built from that same record.
//! - A column's `DEFAULT` expression is only substituted for an
//!   explicit `NULL` under `OR REPLACE` when that `NULL` is a literal
//!   in the `INSERT` statement itself; a parameter/expression that
//!   merely evaluates to `NULL` at runtime is not distinguished from
//!   an ordinary `NOT NULL` violation.

use crate::codegen::expr::{column_index, compile_cond, compile_value};
use crate::codegen::index_maintenance::{emit_index_key_ops, open_index_cursors};
use crate::codegen::select::{
    compile_select_scan, select_result_column_count, CodegenError, ScanCursors,
};
use crate::codegen::{CondTargets, Emitter, Label, NullTarget, RegAlloc, Target};
use crate::parser::ast::{
    ColumnConstraint, ConflictAction, DefaultValue, Expr, ExprKind, Insert, InsertSource, Literal,
    TableConstraint,
};
use crate::parser::error::ParseOutcome;
use crate::parser::parse_create_table;
use crate::schema::{rowid_alias_column, TableSchema};
use crate::vdbe::{affinity_of, Instruction, Opcode, Program, P4};

const TABLE_CURSOR: i32 = 0;
const CHECK_CURSOR: i32 = 1;
const FIRST_INDEX_CURSOR: i32 = 2;

// SQLite's own extended result codes (sqlite3.h) — nothing in this
// codebase defines these yet (grep turns up nothing), so they're
// introduced here rather than borrowed from an existing constant.
pub(crate) const SQLITE_CONSTRAINT_NOTNULL: i32 = 1299;
const SQLITE_CONSTRAINT_PRIMARYKEY: i32 = 1555;
pub(crate) const SQLITE_CONSTRAINT_CHECK: i32 = 275;
// #207: non-rowid UNIQUE violation, enforced via the new `NoConflict`
// real-index seek+branch primitive (`src/vdbe/cursor.rs::no_conflict`).
const SQLITE_CONSTRAINT_UNIQUE: i32 = 2067;

#[derive(Debug, Default)]
pub(crate) struct ColumnPlan {
    pub(crate) not_null: bool,
    pub(crate) default: Option<Expr>,
    pub(crate) checks: Vec<Expr>,
}

fn is_null_literal(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::Literal(Literal::Null))
}

/// A single column's value, from wherever `compile_row` was told to get
/// it: a literal-or-computed `Expr` it must still compile itself
/// (`VALUES`/`DEFAULT VALUES`), or a register a `SELECT` scan already
/// populated (#208) — the latter is never statically known to be a
/// `NULL` literal, since its value isn't known until runtime. Owns its
/// `Expr` (rather than borrowing it) so this type carries no lifetime
/// parameter, per this codebase's qualified-subset gate (`make
/// mvl-limit`) — `Expr` is cheap enough to clone (used the same way
/// throughout `insert.rs`/`select.rs` already, e.g. `ColumnPlan`'s own
/// `checks: Vec<Expr>`).
#[derive(Debug, Clone)]
enum ColumnSource {
    Expr(Expr),
    Reg(i32),
}

/// Resolves a column's value into a *freshly allocated* register,
/// mirroring `compile_value`'s own contract: every column in
/// `compile_row`'s Pass 2 must bump-allocate exactly one new register,
/// so `col_regs` stays the contiguous run `MakeRecord` requires. A
/// `SELECT`-sourced register (`ColumnSource::Reg`) is therefore never
/// returned verbatim — it's copied (`Opcode::Copy`, #208) into a new
/// register instead, since it may sit anywhere in the scan's own
/// register layout (reordered by an explicit target column list,
/// interleaved with other columns, etc.), not necessarily where this
/// column needs to land.
fn compile_column_source(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    schema: &TableSchema,
    cursor: i32,
    source: &ColumnSource,
) -> Result<i32, CodegenError> {
    match source {
        ColumnSource::Expr(expr) => {
            compile_value(em, reg, &crate::codegen::Scope::single(schema, cursor), expr)
        }
        ColumnSource::Reg(src) => {
            let dest = reg.alloc();
            em.emit(Instruction::new(Opcode::Copy, *src, dest, 0));
            Ok(dest)
        }
    }
}

/// Emits `NewRowid` for `schema`'s table cursor into register `dest`.
/// When `is_autoincrement`, sets `P5`/`P4` so the opcode also
/// consults/bumps `sqlite_sequence` (see `NewRowid`'s own doc in
/// `src/vdbe/cursor.rs`) — without this, `INTEGER PRIMARY KEY
/// AUTOINCREMENT` compiled identically to a plain rowid alias and could
/// reuse a rowid after every row referencing it was deleted.
fn emit_new_rowid(em: &mut Emitter, dest: i32, schema: &TableSchema, is_autoincrement: bool) {
    if is_autoincrement {
        let mut instr = Instruction::with_p4(
            Opcode::NewRowid,
            TABLE_CURSOR,
            dest,
            0,
            P4::Str(schema.name.clone()),
        );
        instr.p5 = 1;
        em.emit(instr);
    } else {
        em.emit(Instruction::new(Opcode::NewRowid, TABLE_CURSOR, dest, 0));
    }
}

/// Compiles `insert` against `schema` (the resolved target table) into
/// a `Program`. `select_schema` is only consulted when `insert.source`
/// is `InsertSource::Select` — it's the resolved schema for that
/// `SELECT`'s `FROM` table (#208); pass `None` if the caller couldn't
/// resolve it (or `insert.source` isn't `Select`).
pub fn compile_insert(
    insert: &Insert,
    schema: &TableSchema,
    select_schema: Option<&TableSchema>,
) -> Result<Program, CodegenError> {
    if schema.without_rowid {
        return Err(CodegenError::Unsupported {
            reason: "WITHOUT ROWID tables are not supported by INSERT codegen yet".to_string(),
        });
    }

    let create = match parse_create_table(&schema.sql) {
        ParseOutcome::Accepted(create) => *create,
        ParseOutcome::Unsupported { message, .. } | ParseOutcome::Invalid { message, .. } => {
            return Err(CodegenError::Unsupported {
                reason: format!("could not recover constraints from schema DDL: {message}"),
            })
        }
    };

    let rowid_alias = rowid_alias_column(schema);
    let plans = column_plans(schema, &create, rowid_alias);
    // `AUTOINCREMENT` only ever attaches to the rowid-alias column's
    // own `INTEGER PRIMARY KEY` declaration (SQLite grammar doesn't
    // allow it on a table-level `PRIMARY KEY(...)`), so it's enough to
    // check that one column's constraints.
    let is_autoincrement = rowid_alias.is_some_and(|idx| {
        schema
            .columns
            .get(idx)
            .and_then(|name| {
                create
                    .columns
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(name))
            })
            .is_some_and(|def| {
                def.constraints.iter().any(|c| {
                    matches!(
                        c,
                        ColumnConstraint::PrimaryKey {
                            autoincrement: true,
                            ..
                        }
                    )
                })
            })
    });
    let table_checks: Vec<Expr> = create
        .constraints
        .iter()
        .filter_map(|c| match c {
            TableConstraint::Check(expr) => Some(expr.clone()),
            TableConstraint::PrimaryKey(_) | TableConstraint::Unique(_) => None,
        })
        .collect();

    let target_columns: Vec<usize> = match &insert.columns {
        Some(names) => {
            let mut out = Vec::with_capacity(names.len());
            for name in names {
                let idx = column_index(schema, name)
                    .ok_or_else(|| CodegenError::UnknownColumn { name: name.clone() })?;
                out.push(idx);
            }
            out
        }
        None => (0..schema.columns.len()).collect(),
    };

    if let InsertSource::Values(rows) = &insert.source {
        for row in rows {
            if !row.is_empty() && row.len() != target_columns.len() {
                return Err(CodegenError::RowShapeMismatch {
                    table: schema.name.clone(),
                    expected: target_columns.len(),
                    found: row.len(),
                });
            }
        }
    }
    // Validated up front (rather than inline in the `Select` compile arm
    // below) so a missing `select_schema` or a row-shape mismatch is
    // reported before any code is emitted, matching the literal-`VALUES`
    // check above.
    if let InsertSource::Select(select) = &insert.source {
        let found = match select_schema {
            Some(select_schema) => select_result_column_count(select, select_schema),
            None => {
                return Err(CodegenError::Unsupported {
                    reason: "INSERT ... SELECT: the SELECT's FROM table schema was not resolved"
                        .to_string(),
                })
            }
        };
        if found != target_columns.len() {
            return Err(CodegenError::RowShapeMismatch {
                table: schema.name.clone(),
                expected: target_columns.len(),
                found,
            });
        }
    }

    let action = insert.or_action.unwrap_or(ConflictAction::Abort);

    // Column references inside `CHECK` expressions resolve against
    // this shadow schema instead of `schema` itself: same columns, but
    // no cached DDL text, so `rowid_alias_column` (textual, driven by
    // `sql`) can never fire and make `compile_cond`/`compile_value`
    // emit `Opcode::Rowid` — which the `CHECK` pseudo-cursor (built
    // from a plain record blob, not a real table cursor) doesn't
    // support. Every `CHECK` column reference reads via ordinary
    // `Opcode::Column` instead.
    let check_schema = TableSchema {
        sql: String::new(),
        ..schema.clone()
    };

    let mut em = Emitter::new();
    let mut reg = RegAlloc::new();

    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    em.emit(Instruction::new(
        Opcode::OpenWrite,
        TABLE_CURSOR,
        i32::try_from(schema.root_page).unwrap_or(0),
        0,
    ));
    open_index_cursors(&mut em, schema, FIRST_INDEX_CURSOR)?;

    match &insert.source {
        InsertSource::Values(rows) => {
            for values in rows {
                compile_row(
                    &mut em,
                    &mut reg,
                    schema,
                    &check_schema,
                    &plans,
                    &table_checks,
                    &target_columns,
                    |pos| values.get(pos).cloned().map(ColumnSource::Expr),
                    rowid_alias,
                    action,
                    is_autoincrement,
                )?;
            }
        }
        InsertSource::DefaultValues => {
            compile_row(
                &mut em,
                &mut reg,
                schema,
                &check_schema,
                &plans,
                &table_checks,
                &target_columns,
                |_pos| None,
                rowid_alias,
                action,
                is_autoincrement,
            )?;
        }
        InsertSource::Select(select) => {
            // Re-checked (rather than trusted from the validation above)
            // so this arm never needs an infallible unwrap: `Option` is
            // `Copy` for a `&TableSchema`, so this is the same value,
            // not a second lookup.
            let Some(select_schema) = select_schema else {
                return Err(CodegenError::Unsupported {
                    reason: "INSERT ... SELECT: the SELECT's FROM table schema was not resolved"
                        .to_string(),
                });
            };
            // The select's source-table scan needs cursor numbers of its
            // own, distinct from this INSERT's target-table/index
            // cursors above — offset above the last index cursor
            // (`open_index_cursors` uses `FIRST_INDEX_CURSOR..
            // FIRST_INDEX_CURSOR+schema.indexes.len()`).
            let select_table_cursor =
                FIRST_INDEX_CURSOR.saturating_add(i32::try_from(schema.indexes.len()).unwrap_or(0));
            let select_cursors = ScanCursors {
                table: select_table_cursor,
                sort: select_table_cursor.saturating_add(1),
                pseudo: select_table_cursor.saturating_add(2),
                distinct: select_table_cursor.saturating_add(3),
            };
            em.emit(Instruction::new(
                Opcode::OpenRead,
                select_cursors.table,
                i32::try_from(select_schema.root_page).unwrap_or(0),
                0,
            ));
            let end_label = em.new_label();
            let mut sink = |em: &mut Emitter, reg: &mut RegAlloc, first: i32, count: i32| {
                let count = usize::try_from(count).unwrap_or(0);
                compile_row(
                    em,
                    reg,
                    schema,
                    &check_schema,
                    &plans,
                    &table_checks,
                    &target_columns,
                    |pos| {
                        (pos < count)
                            .then(|| first.saturating_add(i32::try_from(pos).unwrap_or(i32::MAX)))
                            .map(ColumnSource::Reg)
                    },
                    rowid_alias,
                    action,
                    is_autoincrement,
                )
            };
            compile_select_scan(
                &mut em,
                &mut reg,
                select,
                select_schema,
                select_cursors,
                end_label,
                &mut sink,
            )?;
            em.place(end_label);
        }
    }

    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    Ok(em.finish())
}

/// Builds each schema column's constraint facts from the re-parsed
/// `CREATE TABLE` AST, keyed by schema position (column name matched
/// case-insensitively — the same convention `column_index` uses).
pub(crate) fn column_plans(
    schema: &TableSchema,
    create: &crate::parser::ast::CreateTable,
    rowid_alias: Option<usize>,
) -> Vec<ColumnPlan> {
    let mut plans: Vec<ColumnPlan> = schema
        .columns
        .iter()
        .map(|_| ColumnPlan::default())
        .collect();
    for (idx, col_name) in schema.columns.iter().enumerate() {
        let Some(def) = create
            .columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(col_name))
        else {
            continue;
        };
        let Some(plan) = plans.get_mut(idx) else {
            continue;
        };
        for constraint in &def.constraints {
            match constraint {
                ColumnConstraint::NotNull => plan.not_null = true,
                // NULL on the rowid-alias column means "assign a fresh
                // rowid", not a constraint violation, so its implicit
                // NOT NULL is deliberately not enforced.
                ColumnConstraint::PrimaryKey { .. } => {
                    if rowid_alias != Some(idx) {
                        plan.not_null = true;
                    }
                }
                ColumnConstraint::Default(
                    DefaultValue::Literal(expr) | DefaultValue::Paren(expr),
                ) => {
                    plan.default = Some(expr.clone());
                }
                ColumnConstraint::Check(expr) => plan.checks.push(expr.clone()),
                ColumnConstraint::Unique | ColumnConstraint::Collate(_) => {}
            }
        }
    }
    plans
}

/// `column_at(pos)` resolves the value supplied for the `pos`-th
/// position in `insert.columns`' (or the schema's default) order — a
/// literal `VALUES`/`DEFAULT VALUES` row's `Expr` at that position, or
/// (#208) the SELECT-projected row's `pos`-th register — generic
/// (rather than a boxed/`dyn` closure) per this codebase's
/// qualified-subset gate (`make mvl-limit`).
#[allow(clippy::too_many_arguments)]
fn compile_row(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    schema: &TableSchema,
    check_schema: &TableSchema,
    plans: &[ColumnPlan],
    table_checks: &[Expr],
    target_columns: &[usize],
    mut column_at: impl FnMut(usize) -> Option<ColumnSource>,
    rowid_alias: Option<usize>,
    action: ConflictAction,
    is_autoincrement: bool,
) -> Result<(), CodegenError> {
    let mut value_sources: Vec<Option<ColumnSource>> = vec![None; schema.columns.len()];
    for (pos, &col_idx) in target_columns.iter().enumerate() {
        if let (Some(source), Some(slot)) = (column_at(pos), value_sources.get_mut(col_idx)) {
            *slot = Some(source);
        }
    }
    let value_source_at =
        |idx: usize| -> Option<ColumnSource> { value_sources.get(idx).and_then(Option::clone) };

    let row_skip = em.new_label();

    // Pass 1: resolve the rowid before touching any column's register,
    // so its register(s) never fall inside the contiguous run `col_regs`
    // needs for `MakeRecord` below.
    let rowid_reg = match rowid_alias {
        Some(idx) => {
            let explicit = value_source_at(idx)
                .filter(|source| !matches!(source, ColumnSource::Expr(e) if is_null_literal(e)));
            match explicit {
                Some(source) => {
                    let r = compile_column_source(em, reg, schema, TABLE_CURSOR, &source)?;
                    let no_conflict = em.new_label();
                    let seek_addr =
                        em.emit(Instruction::new(Opcode::SeekRowid, TABLE_CURSOR, 0, r));
                    em.patch_p2(seek_addr, no_conflict);
                    emit_pk_conflict(em, reg, action, row_skip, schema, idx)?;
                    em.place(no_conflict);
                    r
                }
                None => {
                    let r = reg.alloc();
                    emit_new_rowid(em, r, schema, is_autoincrement);
                    r
                }
            }
        }
        None => {
            let r = reg.alloc();
            emit_new_rowid(em, r, schema, is_autoincrement);
            r
        }
    };

    // Pass 2: exactly one register per schema column, in order — the
    // contiguous run `MakeRecord` reads.
    let mut col_regs = Vec::with_capacity(schema.columns.len());
    for (idx, plan) in plans.iter().enumerate() {
        if Some(idx) == rowid_alias {
            // Matches stock SQLite: the rowid-alias column is always
            // stored as NULL in the record; readers substitute the
            // cursor's actual rowid instead (see
            // `crate::codegen::expr::emit_column_read`).
            let r = reg.alloc();
            em.emit(Instruction::new(Opcode::Null, 0, r, 0));
            col_regs.push(r);
            continue;
        }

        let provided = value_source_at(idx);
        let use_default = provided.is_none()
            || (matches!(&provided, Some(ColumnSource::Expr(e)) if is_null_literal(e))
                && action == ConflictAction::Replace
                && plan.default.is_some());
        let chosen: Option<ColumnSource> = if use_default {
            plan.default.clone().map(ColumnSource::Expr).or(provided)
        } else {
            provided
        };

        let r = match &chosen {
            Some(source) => compile_column_source(em, reg, schema, TABLE_CURSOR, source)?,
            None => {
                let r = reg.alloc();
                em.emit(Instruction::new(Opcode::Null, 0, r, 0));
                r
            }
        };
        col_regs.push(r);

        if plan.not_null {
            let violation = em.new_label();
            let ok = em.new_label();
            let addr = em.emit(Instruction::new(Opcode::IsNull, r, 0, 0));
            em.patch_p2(addr, violation);
            em.goto(ok);
            em.place(violation);
            emit_constraint_violation(
                em,
                action,
                SQLITE_CONSTRAINT_NOTNULL,
                format!(
                    "NOT NULL constraint failed: {}.{}",
                    schema.name,
                    schema.columns.get(idx).map_or("?", String::as_str)
                ),
                row_skip,
            );
            em.place(ok);
        }
    }

    let unique_indexes: Vec<(i32, &crate::schema::IndexSchema)> = schema
        .indexes
        .iter()
        .enumerate()
        .filter(|(_, idx)| idx.unique)
        .map(|(i, idx)| {
            (
                FIRST_INDEX_CURSOR.saturating_add(i32::try_from(i).unwrap_or(0)),
                idx,
            )
        })
        .collect();

    let has_checks = !table_checks.is_empty() || plans.iter().any(|p| !p.checks.is_empty());
    let needs_row_pseudo = has_checks || !unique_indexes.is_empty();
    if needs_row_pseudo {
        let base_reg = col_regs.first().copied().unwrap_or(0);
        let count = i32::try_from(col_regs.len()).unwrap_or(0);
        let check_record_reg = reg.alloc();
        em.emit(Instruction::new(
            Opcode::MakeRecord,
            base_reg,
            count,
            check_record_reg,
        ));
        em.emit(Instruction::new(
            Opcode::OpenPseudo,
            CHECK_CURSOR,
            check_record_reg,
            0,
        ));
    }

    if has_checks {
        let mut check_exprs: Vec<&Expr> = plans.iter().flat_map(|p| p.checks.iter()).collect();
        check_exprs.extend(table_checks.iter());
        for expr in check_exprs {
            let violation = em.new_label();
            let ok = em.new_label();
            compile_cond(
                em,
                reg,
                &crate::codegen::Scope::single(check_schema, CHECK_CURSOR),
                expr,
                CondTargets {
                    on_true: Target::Fallthrough,
                    on_false: Target::Jump(violation),
                    on_null: NullTarget::True,
                },
            )?;
            em.goto(ok);
            em.place(violation);
            emit_constraint_violation(
                em,
                action,
                SQLITE_CONSTRAINT_CHECK,
                format!("CHECK constraint failed: {}", schema.name),
                row_skip,
            );
            em.place(ok);
        }
    }

    for (index_cursor, index) in &unique_indexes {
        emit_unique_check(
            em,
            reg,
            schema,
            check_schema,
            *index_cursor,
            index,
            action,
            row_skip,
        )?;
    }

    let base_reg = col_regs.first().copied().unwrap_or(0);
    let count = i32::try_from(col_regs.len()).unwrap_or(0);
    let record_reg = reg.alloc();
    let affinities: Vec<u8> = schema
        .column_types
        .iter()
        .map(|t| affinity_of(t).to_p4_byte())
        .collect();
    em.emit(Instruction::with_p4(
        Opcode::MakeRecord,
        base_reg,
        count,
        record_reg,
        P4::Affinity(affinities),
    ));
    em.emit(Instruction::new(
        Opcode::Insert,
        TABLE_CURSOR,
        rowid_reg,
        record_reg,
    ));

    if !schema.indexes.is_empty() {
        // `Insert` doesn't reposition `TABLE_CURSOR` onto the row it
        // just wrote, but the index-key registers are read back via
        // `Opcode::Column`/`Opcode::Rowid` against the cursor's current
        // row (see `index_maintenance`), so seek onto it first. A
        // not-found jump target is required by `SeekRowid`'s shape but
        // should be unreachable — the row was just inserted.
        let seek_ok = em.new_label();
        let seek_addr = em.emit(Instruction::new(
            Opcode::SeekRowid,
            TABLE_CURSOR,
            0,
            rowid_reg,
        ));
        em.patch_p2(seek_addr, seek_ok);
        emit_index_key_ops(
            em,
            reg,
            schema,
            TABLE_CURSOR,
            FIRST_INDEX_CURSOR,
            Opcode::IdxInsert,
        )?;
        em.place(seek_ok);
    }

    em.place(row_skip);
    Ok(())
}

pub(crate) fn emit_constraint_violation(
    em: &mut Emitter,
    action: ConflictAction,
    code: i32,
    message: String,
    row_skip: Label,
) {
    match action {
        ConflictAction::Ignore => em.goto(row_skip),
        ConflictAction::Abort
        | ConflictAction::Fail
        | ConflictAction::Rollback
        | ConflictAction::Replace => {
            em.emit(Instruction::with_p4(
                Opcode::Halt,
                code,
                0,
                0,
                P4::Str(message),
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_pk_conflict(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    action: ConflictAction,
    row_skip: Label,
    schema: &TableSchema,
    idx: usize,
) -> Result<(), CodegenError> {
    match action {
        ConflictAction::Ignore => em.goto(row_skip),
        ConflictAction::Replace => {
            // `SeekRowid` above landed the cursor on the row this
            // INSERT is about to displace — its secondary-index
            // entries must be removed before it's deleted, exactly
            // like `delete.rs`'s ordinary `DELETE` path, or they go
            // stale (`#196` follow-up: this conflict path predates
            // index maintenance and wasn't updated when it landed).
            emit_index_key_ops(
                em,
                reg,
                schema,
                TABLE_CURSOR,
                FIRST_INDEX_CURSOR,
                Opcode::IdxDelete,
            )?;
            em.emit(Instruction::new(Opcode::Delete, TABLE_CURSOR, 0, 0));
        }
        ConflictAction::Abort | ConflictAction::Fail | ConflictAction::Rollback => {
            let message = format!(
                "UNIQUE constraint failed: {}.{} (PRIMARY KEY)",
                schema.name,
                schema.columns.get(idx).map_or("?", String::as_str)
            );
            em.emit(Instruction::with_p4(
                Opcode::Halt,
                SQLITE_CONSTRAINT_PRIMARYKEY,
                0,
                0,
                P4::Str(message),
            ));
        }
    }
    Ok(())
}

/// Enforces one non-rowid UNIQUE index (#207) against the row currently
/// staged in `col_regs` (read back via `CHECK_CURSOR`'s pseudo cursor,
/// same trick `has_checks` uses — see `compile_row`). Emits the probe
/// key (the index's declared columns, in order) into a fresh contiguous
/// register run, reserves one more register for `Opcode::NoConflict`'s
/// conflicting-rowid output, then dispatches on `action` exactly like
/// `emit_pk_conflict` does for the rowid-PK case.
#[allow(clippy::too_many_arguments)]
fn emit_unique_check(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    schema: &TableSchema,
    check_schema: &TableSchema,
    index_cursor: i32,
    index: &crate::schema::IndexSchema,
    action: ConflictAction,
    row_skip: Label,
) -> Result<(), CodegenError> {
    let mut start = None;
    let mut key_col_indices = Vec::with_capacity(index.columns.len());
    for col in &index.columns {
        if col.desc {
            return Err(CodegenError::Unsupported {
                reason: format!(
                    "index {} has a DESC column ({}); descending index keys aren't supported yet",
                    index.name, col.name
                ),
            });
        }
        let col_idx = column_index(schema, &col.name).ok_or_else(|| CodegenError::Unsupported {
            reason: format!(
                "index {} references a column or expression this codegen can't resolve: {}",
                index.name, col.name
            ),
        })?;
        key_col_indices.push(col_idx);
        let r = reg.alloc();
        if start.is_none() {
            start = Some(r);
        }
        crate::codegen::expr::emit_column_read(em, check_schema, CHECK_CURSOR, col_idx, r)?;
    }
    let start = start.unwrap_or(0);
    let count = i32::try_from(key_col_indices.len()).unwrap_or(0);
    // `NoConflict`'s contract (`src/vdbe/cursor.rs::no_conflict`): the
    // register immediately after the probe range receives the
    // conflicting row's rowid on a fallthrough (conflict) — reserved
    // here even when `action` doesn't need it (`Ignore`/`Abort`/etc.),
    // to keep the register layout uniform.
    let conflict_rowid_reg = reg.alloc();

    let no_conflict = em.new_label();
    let addr = em.emit(Instruction::with_p4(
        Opcode::NoConflict,
        index_cursor,
        0,
        start,
        P4::Int(count.into()),
    ));
    em.patch_p2(addr, no_conflict);

    match action {
        ConflictAction::Ignore => em.goto(row_skip),
        ConflictAction::Replace => {
            // The conflicting row's rowid was written into
            // `conflict_rowid_reg` by `NoConflict` itself — seek the
            // table cursor onto it, remove its index entries, then
            // delete it, exactly like `emit_pk_conflict`'s `Replace`
            // branch (which instead already had the cursor positioned
            // via `SeekRowid`).
            let seek_ok = em.new_label();
            let seek_addr = em.emit(Instruction::new(
                Opcode::SeekRowid,
                TABLE_CURSOR,
                0,
                conflict_rowid_reg,
            ));
            em.patch_p2(seek_addr, seek_ok);
            crate::codegen::index_maintenance::emit_index_key_ops(
                em,
                reg,
                schema,
                TABLE_CURSOR,
                FIRST_INDEX_CURSOR,
                Opcode::IdxDelete,
            )?;
            em.emit(Instruction::new(Opcode::Delete, TABLE_CURSOR, 0, 0));
            em.place(seek_ok);
        }
        ConflictAction::Abort | ConflictAction::Fail | ConflictAction::Rollback => {
            let message = format!("UNIQUE constraint failed: {}.{}", schema.name, index.name);
            em.emit(Instruction::with_p4(
                Opcode::Halt,
                SQLITE_CONSTRAINT_UNIQUE,
                0,
                0,
                P4::Str(message),
            ));
        }
    }
    em.place(no_conflict);
    Ok(())
}
