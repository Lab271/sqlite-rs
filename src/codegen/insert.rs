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
//! Known simplifications (deferred to follow-up tickets, not chased
//! here):
//! - `InsertSource::Select` does not compile (`VALUES`/`DEFAULT VALUES`
//!   only) — filed separately from #195.
//! - UNIQUE constraints on non-rowid columns still aren't *enforced* as
//!   a constraint violation: a duplicate key surfaces as
//!   `IdxInsert`'s generic `BtreeError::DuplicateKey` ->
//!   `MalformedInstruction`, not a `SQLITE_CONSTRAINT_UNIQUE` /
//!   `ON CONFLICT` outcome. Filed as a follow-up.
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
use crate::codegen::select::CodegenError;
use crate::codegen::{CondTargets, Emitter, Label, NullTarget, RegAlloc, Target};
use crate::parser::ast::{
    ColumnConstraint, ConflictAction, DefaultValue, Expr, ExprKind, Insert, InsertSource, Literal,
    TableConstraint,
};
use crate::parser::error::CreateTableOutcome;
use crate::parser::parse_create_table;
use crate::schema::{rowid_alias_column, TableSchema};
use crate::vdbe::{affinity_of, Instruction, Opcode, Program, P4};

const TABLE_CURSOR: i32 = 0;
const CHECK_CURSOR: i32 = 1;
const FIRST_INDEX_CURSOR: i32 = 2;

// SQLite's own extended result codes (sqlite3.h) — nothing in this
// codebase defines these yet (grep turns up nothing), so they're
// introduced here rather than borrowed from an existing constant.
const SQLITE_CONSTRAINT_NOTNULL: i32 = 1299;
const SQLITE_CONSTRAINT_PRIMARYKEY: i32 = 1555;
const SQLITE_CONSTRAINT_CHECK: i32 = 275;

#[derive(Debug, Default)]
struct ColumnPlan {
    not_null: bool,
    default: Option<Expr>,
    checks: Vec<Expr>,
}

fn is_null_literal(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::Literal(Literal::Null))
}

/// Compiles `insert` against `schema` (the resolved target table) into
/// a `Program`.
pub fn compile_insert(insert: &Insert, schema: &TableSchema) -> Result<Program, CodegenError> {
    if schema.without_rowid {
        return Err(CodegenError::Unsupported {
            reason: "WITHOUT ROWID tables are not supported by INSERT codegen yet".to_string(),
        });
    }

    let create = match parse_create_table(&schema.sql) {
        CreateTableOutcome::Accepted(create) => *create,
        CreateTableOutcome::Unsupported { message, .. }
        | CreateTableOutcome::Invalid { message, .. } => {
            return Err(CodegenError::Unsupported {
                reason: format!("could not recover constraints from schema DDL: {message}"),
            })
        }
    };

    let rowid_alias = rowid_alias_column(schema);
    let plans = column_plans(schema, &create, rowid_alias);
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

    let rows: Vec<Vec<Expr>> = match &insert.source {
        InsertSource::Values(rows) => rows.clone(),
        InsertSource::DefaultValues => vec![Vec::new()],
        InsertSource::Select(_) => {
            return Err(CodegenError::Unsupported {
                reason: "INSERT ... SELECT is not supported by codegen yet".to_string(),
            })
        }
    };

    for row in &rows {
        if !row.is_empty() && row.len() != target_columns.len() {
            return Err(CodegenError::RowShapeMismatch {
                table: schema.name.clone(),
                expected: target_columns.len(),
                found: row.len(),
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

    for values in &rows {
        compile_row(
            &mut em,
            &mut reg,
            schema,
            &check_schema,
            &plans,
            &table_checks,
            &target_columns,
            values,
            rowid_alias,
            action,
        )?;
    }

    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    Ok(em.finish())
}

/// Builds each schema column's constraint facts from the re-parsed
/// `CREATE TABLE` AST, keyed by schema position (column name matched
/// case-insensitively — the same convention `column_index` uses).
fn column_plans(
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

#[allow(clippy::too_many_arguments)]
fn compile_row(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    schema: &TableSchema,
    check_schema: &TableSchema,
    plans: &[ColumnPlan],
    table_checks: &[Expr],
    target_columns: &[usize],
    values: &[Expr],
    rowid_alias: Option<usize>,
    action: ConflictAction,
) -> Result<(), CodegenError> {
    let mut value_exprs: Vec<Option<&Expr>> = vec![None; schema.columns.len()];
    for (pos, &col_idx) in target_columns.iter().enumerate() {
        if let (Some(expr), Some(slot)) = (values.get(pos), value_exprs.get_mut(col_idx)) {
            *slot = Some(expr);
        }
    }
    let value_expr_at = |idx: usize| -> Option<&Expr> { value_exprs.get(idx).copied().flatten() };

    let row_skip = em.new_label();

    // Pass 1: resolve the rowid before touching any column's register,
    // so its register(s) never fall inside the contiguous run `col_regs`
    // needs for `MakeRecord` below.
    let rowid_reg = match rowid_alias {
        Some(idx) => {
            let explicit = value_expr_at(idx).filter(|expr| !is_null_literal(expr));
            match explicit {
                Some(expr) => {
                    let r = compile_value(em, reg, schema, TABLE_CURSOR, expr)?;
                    let no_conflict = em.new_label();
                    let seek_addr =
                        em.emit(Instruction::new(Opcode::SeekRowid, TABLE_CURSOR, 0, r));
                    em.patch_p2(seek_addr, no_conflict);
                    emit_pk_conflict(em, action, row_skip, schema, idx);
                    em.place(no_conflict);
                    r
                }
                None => {
                    let r = reg.alloc();
                    em.emit(Instruction::new(Opcode::NewRowid, TABLE_CURSOR, r, 0));
                    r
                }
            }
        }
        None => {
            let r = reg.alloc();
            em.emit(Instruction::new(Opcode::NewRowid, TABLE_CURSOR, r, 0));
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

        let provided = value_expr_at(idx);
        let use_default = provided.is_none()
            || (matches!(provided, Some(expr) if is_null_literal(expr))
                && action == ConflictAction::Replace
                && plan.default.is_some());
        let chosen: Option<&Expr> = if use_default {
            plan.default.as_ref().or(provided)
        } else {
            provided
        };

        let r = match chosen {
            Some(expr) => compile_value(em, reg, schema, TABLE_CURSOR, expr)?,
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

    let has_checks = !table_checks.is_empty() || plans.iter().any(|p| !p.checks.is_empty());
    if has_checks {
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

        let mut check_exprs: Vec<&Expr> = plans.iter().flat_map(|p| p.checks.iter()).collect();
        check_exprs.extend(table_checks.iter());
        for expr in check_exprs {
            let violation = em.new_label();
            let ok = em.new_label();
            compile_cond(
                em,
                reg,
                check_schema,
                CHECK_CURSOR,
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

fn emit_constraint_violation(
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

fn emit_pk_conflict(
    em: &mut Emitter,
    action: ConflictAction,
    row_skip: Label,
    schema: &TableSchema,
    idx: usize,
) {
    match action {
        ConflictAction::Ignore => em.goto(row_skip),
        ConflictAction::Replace => {
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
}
