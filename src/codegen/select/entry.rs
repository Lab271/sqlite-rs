use super::aggregate::{
    compile_grouped_scan, select_has_aggregate, try_compile_index_ordered_group_by,
};
use super::index_scan::try_compile_index_ordered_scan;
use super::limit_scan::{compile_direct_scan, compile_sorted_scan};
use super::order_by::resolve_order_by;
use super::projection::{compile_row_values, result_columns};
use super::*;
use crate::codegen::index_maintenance::valid_table_root_page;
/// Compiles `select` against `schema` (the resolved `FROM` table) into
/// a `Program`. Single-table only — a `select.from` with a non-empty
/// `joins` list (#237) has more than one table to resolve schemas for,
/// which this single-`schema` signature has no way to accept; use
/// [`compile_select_joined`] instead. A `FROM` table that's a subquery
/// (#257) is materialized into an ephemeral table — `schema` in that
/// case must be the caller-resolved synthetic schema describing the
/// subquery's own projected columns, not a catalog lookup.
pub fn compile_select(select: &Select, schema: &TableSchema) -> Result<Program, CodegenError> {
    compile_select_with_catalog(select, schema, std::slice::from_ref(schema))
}

/// [`compile_select`], plus `catalog` — the full table catalog (#238),
/// used to resolve a scalar/`IN`/`EXISTS` subquery expression's own
/// `FROM` table when it names a table other than `schema` itself.
/// `compile_select` is the common case (no cross-table subquery
/// support needed, or a subquery that only ever selects from `schema`
/// itself) and just calls through with `catalog = [schema]`.
pub fn compile_select_with_catalog(
    select: &Select,
    schema: &TableSchema,
    catalog: &[TableSchema],
) -> Result<Program, CodegenError> {
    let Some(from) = &select.from else {
        return compile_select_no_from(select, catalog);
    };
    if !from.joins.is_empty() {
        return Err(CodegenError::Unsupported {
            reason: "this SELECT's FROM clause has a JOIN — call compile_select_joined with \
                     every joined table's schema instead of compile_select"
                .to_string(),
        });
    }
    if !select.compound.is_empty() {
        return Err(CodegenError::Unsupported {
            reason: "this SELECT is a UNION compound — call compile_select_compound instead"
                .to_string(),
        });
    }

    let mut em = Emitter::new();
    let mut reg = RegAlloc::new();

    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    let cursors = ScanCursors::for_standalone_select();
    match &from.first.kind {
        TableRefKind::Name(_) => {
            let root_page = valid_table_root_page(schema)?;
            em.emit(Instruction::new(
                Opcode::OpenRead,
                cursors.table,
                root_page,
                0,
            ));
        }
        TableRefKind::Subquery(subquery) => {
            crate::codegen::subquery::materialize_from_subquery(
                &mut em,
                &mut reg,
                subquery,
                catalog,
                cursors.table,
            )?;
        }
    }

    let end_label = em.new_label();
    let mut sink = |em: &mut Emitter, _reg: &mut RegAlloc, first: i32, count: i32| {
        em.emit(Instruction::new(Opcode::ResultRow, first, count, 0));
        Ok(())
    };
    compile_select_scan(
        &mut em, &mut reg, select, schema, cursors, end_label, catalog, &mut sink,
    )?;

    em.place(end_label);
    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));

    Ok(em.finish())
}

/// A FROM-less `SELECT <expr>[, ...]` (#260) — SQLite's normal way to
/// call a zero-arg built-in (`SELECT sqlite_version();`) or evaluate a
/// bare expression (`SELECT 1 + 1;`). No cursor, no scan loop: the
/// column list is compiled once against an empty schema and emitted
/// as exactly one row. `*`/`tbl.*` and any clause that presumes a
/// table (WHERE/GROUP BY/HAVING/ORDER BY/LIMIT/DISTINCT/compound) has
/// nothing to operate over here and is rejected as unsupported rather
/// than silently no-op'd.
pub(super) fn compile_select_no_from(
    select: &Select,
    catalog: &[TableSchema],
) -> Result<Program, CodegenError> {
    if select
        .columns
        .iter()
        .any(|col| !matches!(col, ResultColumn::Expr { .. }))
    {
        return Err(CodegenError::Unsupported {
            reason: "`*`/`tbl.*` has no table to expand in a FROM-less SELECT".to_string(),
        });
    }
    if select.where_clause.is_some()
        || !select.group_by.is_empty()
        || select.having.is_some()
        || !select.order_by.is_empty()
        || select.limit.is_some()
        || select.distinct.is_some()
        || !select.compound.is_empty()
    {
        return Err(CodegenError::Unsupported {
            reason: "a FROM-less SELECT only supports a bare expression list — no WHERE/GROUP \
                     BY/HAVING/ORDER BY/LIMIT/DISTINCT/compound clause"
                .to_string(),
        });
    }

    let mut em = Emitter::new();
    let mut reg = RegAlloc::new();

    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    let no_table = TableSchema {
        name: String::new(),
        root_page: 0,
        columns: vec![],
        column_types: vec![],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: String::new(),
        indexes: vec![],
    };
    let cols = result_columns(select, &no_table);
    let (first, count) =
        compile_row_values(&mut em, &mut reg, &no_table, &cols, -1, false, catalog)?;
    em.emit(Instruction::new(
        Opcode::ResultRow,
        first,
        i32::try_from(count).unwrap_or(0),
        0,
    ));
    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));

    Ok(em.finish())
}

/// The scan/filter/project core of `compile_select`, minus the
/// `Init`/`OpenRead`/`Halt` bracketing — factored out so #208's `INSERT
/// ... SELECT` codegen can drive the same scan (with its own cursor
/// numbers and its own `OpenRead` already emitted) and substitute a
/// different per-row `sink` in place of `ResultRow`. Generic over `sink`
/// (rather than a `dyn FnMut` trait object) per this codebase's
/// qualified-subset gate (`make mvl-limit`) — no dynamic dispatch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_select_scan<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursors: ScanCursors,
    end_label: Label,
    catalog: &[TableSchema],
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    if !select.group_by.is_empty() {
        if !select.order_by.is_empty() {
            return Err(CodegenError::Unsupported {
                reason: "GROUP BY combined with ORDER BY not yet supported".to_string(),
            });
        }
        if select.distinct.is_some() {
            return Err(CodegenError::Unsupported {
                reason: "GROUP BY combined with DISTINCT not yet supported".to_string(),
            });
        }
        if try_compile_index_ordered_group_by(
            em, reg, select, schema, cursors, end_label, catalog, false, sink,
        )? {
            return Ok(());
        }
        return compile_grouped_scan(
            em, reg, select, schema, cursors, end_label, catalog, false, None, sink,
        );
    }
    // #287: no explicit GROUP BY, but the SELECT list/HAVING has an
    // aggregate call — the whole table is one implicit group, reusing
    // `compile_grouped_scan`'s machinery with an empty GROUP BY key
    // (so every row belongs to the same synthetic group) and
    // `implicit_group: true` (so a zero-row table still flushes one
    // row — count(*) = 0, other aggregates NULL — instead of zero
    // rows the way an explicit `GROUP BY` over no matches would).
    if select_has_aggregate(select) {
        if !select.order_by.is_empty() {
            return Err(CodegenError::Unsupported {
                reason: "ORDER BY combined with an aggregate (no GROUP BY) not yet supported"
                    .to_string(),
            });
        }
        if select.distinct.is_some() {
            return Err(CodegenError::Unsupported {
                reason: "DISTINCT combined with an aggregate (no GROUP BY) not yet supported"
                    .to_string(),
            });
        }
        return compile_grouped_scan(
            em, reg, select, schema, cursors, end_label, catalog, true, None, sink,
        );
    }
    if select.having.is_some() {
        return Err(CodegenError::Unsupported {
            reason: "HAVING without GROUP BY not yet supported".to_string(),
        });
    }

    let order_by_plans = resolve_order_by(select, schema)?;
    if order_by_plans.is_empty() {
        compile_direct_scan(em, reg, select, schema, cursors, end_label, catalog, sink)
    } else if try_compile_index_ordered_scan(
        em,
        reg,
        select,
        schema,
        &order_by_plans,
        cursors,
        end_label,
        catalog,
        sink,
    )? {
        Ok(())
    } else {
        compile_sorted_scan(
            em,
            reg,
            select,
            schema,
            &order_by_plans,
            cursors,
            end_label,
            catalog,
            sink,
        )
    }
}

/// The number of columns `select` projects against `schema` — used by
/// #208's `INSERT ... SELECT` codegen to validate row shape against the
/// target column list at compile time, the same way a literal `VALUES`
/// row's length is checked.
pub(crate) fn select_result_column_count(select: &Select, schema: &TableSchema) -> usize {
    result_columns(select, schema).len()
}

/// [`select_result_column_count`]'s joined counterpart (#250's `INSERT
/// ... SELECT` + JOIN): the number of columns `select` projects against
/// `schemas` (one per `FROM` table, same order as `table_refs` — the
/// `FROM` clause's table references, used only to resolve a `table.*`
/// qualifier to its schema). Known narrower-than-ideal scope: unlike
/// `compile_select_joined_scan`'s actual projection, this count doesn't
/// account for NATURAL/USING's `SELECT *` de-duplication (it has no
/// `dedup_star` to consult — building one here would duplicate
/// `compile_select_joined_scan`'s join-constraint synthesis just for a
/// count) — a `SELECT *` combined with `INSERT ... SELECT` across a
/// NATURAL/USING join may over-count relative to what actually gets
/// projected. Ordinary `ON`-constrained joins (this ticket's tested
/// shape) are unaffected.
pub(crate) fn select_result_column_count_joined(
    select: &Select,
    schemas: &[TableSchema],
    table_refs: &[&TableRef],
) -> Result<usize, CodegenError> {
    let mut count = 0usize;
    for col in &select.columns {
        match col {
            ResultColumn::Star => {
                count = count.saturating_add(schemas.iter().map(|s| s.columns.len()).sum());
            }
            ResultColumn::TableStar { table } => {
                let idx = table_refs
                    .iter()
                    .position(|t| {
                        t.alias
                            .as_deref()
                            .or(t.name())
                            .unwrap_or("")
                            .eq_ignore_ascii_case(table)
                    })
                    .ok_or_else(|| CodegenError::UnknownColumn {
                        name: format!("{table}.*"),
                    })?;
                let n = schemas.get(idx).map(|s| s.columns.len()).unwrap_or(0);
                count = count.saturating_add(n);
            }
            ResultColumn::Expr { .. } => count = count.saturating_add(1),
        }
    }
    Ok(count)
}

/// Turns a `UNION ALL` arm into a standalone `Select` so it can be fed
/// through [`select_result_column_count`]/[`compile_select_scan`] the
/// same way the compound's first arm is — `order_by`/`limit` are always
/// empty since those bind to the whole compound statement, not any one
/// arm (see [`crate::parser::ast::Select::compound`]).
pub(super) fn arm_as_select(arm: &CompoundSelect) -> Select {
    Select {
        with_clause: None,
        distinct: arm.distinct,
        columns: arm.columns.clone(),
        from: arm.from.clone(),
        where_clause: arm.where_clause.clone(),
        group_by: arm.group_by.clone(),
        having: arm.having.clone(),
        compound: Vec::new(),
        order_by: Vec::new(),
        limit: None,
        span: arm.span,
    }
}

/// Compiles a `UNION [ALL]` compound `SELECT` (#240 for `UNION ALL`,
/// #377/#378 for plain `UNION`): `first` against `first_schema`, then
/// each of `select.compound`'s arms against its paired schema in
/// `arm_schemas` (same order, one per arm). Each arm gets its own
/// `OpenRead`/scan/`ResultRow` block with cursor numbers offset by
/// `ScanCursors::for_arm`, so arms never collide even when an arm
/// itself uses a sort or DISTINCT cursor. `first`'s `order_by`/`limit`
/// apply to the whole compound statement, but are not yet implemented
/// here — sorting/limiting a concatenation of independent scans needs
/// a shared sorter across arms, which is out of this ticket's scope;
/// callers must reject a non-empty `order_by`/`limit` before calling
/// this.
///
/// If any arm's `op` is `CompoundOp::Union`, every row from every arm
/// (not just that arm's own) is routed through one ephemeral index
/// opened once for the whole statement, reusing the exact
/// `Found`/`IdxInsert` dedup check `SELECT DISTINCT` already performs
/// (`projection::emit_dedup_guard`) — a row already seen from an
/// earlier arm is silently dropped instead of re-emitted. Mixing
/// `UNION` and `UNION ALL` arms in one statement (rare in practice) is
/// simplified to "any `UNION` arm dedups the whole result", rather
/// than SQLite's pairwise left-to-right operator semantics — a known,
/// documented narrowing rather than the general case.
///
/// Joins/subqueries within any arm are out of scope for this ticket —
/// every arm's `from` must be a single table with no joins.
pub fn compile_select_compound(
    first: &Select,
    first_schema: &TableSchema,
    arm_schemas: &[TableSchema],
    catalog: &[TableSchema],
) -> Result<Program, CodegenError> {
    if first
        .from
        .as_ref()
        .is_some_and(|from| !from.joins.is_empty())
    {
        return Err(CodegenError::Unsupported {
            reason: "UNION with a JOIN in one of its arms is not yet supported".to_string(),
        });
    }
    if first.compound.len() != arm_schemas.len() {
        return Err(CodegenError::Unsupported {
            reason: "compile_select_compound: arm_schemas must have one entry per compound arm"
                .to_string(),
        });
    }
    if !first.order_by.is_empty() || first.limit.is_some() {
        return Err(CodegenError::Unsupported {
            reason: "ORDER BY/LIMIT on a UNION compound SELECT is not yet supported".to_string(),
        });
    }

    let expected = select_result_column_count(first, first_schema);
    let mut arm_selects = Vec::with_capacity(first.compound.len());
    for (arm, arm_schema) in first.compound.iter().zip(arm_schemas) {
        if arm.from.as_ref().is_some_and(|from| !from.joins.is_empty()) {
            return Err(CodegenError::Unsupported {
                reason: "UNION with a JOIN in one of its arms is not yet supported".to_string(),
            });
        }
        let arm_select = arm_as_select(arm);
        let found = select_result_column_count(&arm_select, arm_schema);
        if found != expected {
            return Err(CodegenError::CompoundColumnMismatch { expected, found });
        }
        arm_selects.push(arm_select);
    }

    let has_union = first
        .compound
        .iter()
        .any(|arm| arm.op == CompoundOp::Union);
    // One cursor block per arm (`ScanCursors::for_arm`, 4 cursors
    // each) — the shared dedup cursor sits right past the last arm's
    // block so it never collides with any arm's own table/sort/pseudo/
    // distinct cursors.
    let dedup_cursor = ScanCursors::after_arms(arm_schemas.len().saturating_add(1));

    let mut em = Emitter::new();
    let mut reg = RegAlloc::new();

    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    if has_union {
        em.emit(Instruction::new(Opcode::OpenEphemeral, dedup_cursor, 0, 0));
    }

    let mut sink = |em: &mut Emitter, _reg: &mut RegAlloc, reg_first: i32, count: i32| {
        if has_union {
            let found_addr = em.emit(Instruction::with_p4(
                Opcode::Found,
                dedup_cursor,
                0,
                reg_first,
                P4::Int(i64::from(count)),
            ));
            let skip = em.new_label();
            em.patch_p2(found_addr, skip);
            em.emit(Instruction::with_p4(
                Opcode::IdxInsert,
                dedup_cursor,
                reg_first,
                0,
                P4::Int(i64::from(count)),
            ));
            em.emit(Instruction::new(Opcode::ResultRow, reg_first, count, 0));
            em.place(skip);
        } else {
            em.emit(Instruction::new(Opcode::ResultRow, reg_first, count, 0));
        }
        Ok(())
    };

    let mut compile_arm = |em: &mut Emitter,
                           reg: &mut RegAlloc,
                           arm_index: usize,
                           select: &Select,
                           schema: &TableSchema|
     -> Result<(), CodegenError> {
        let cursors = ScanCursors::for_arm(arm_index);
        let root_page = valid_table_root_page(schema)?;
        em.emit(Instruction::new(
            Opcode::OpenRead,
            cursors.table,
            root_page,
            0,
        ));
        let arm_end = em.new_label();
        compile_select_scan(
            em, reg, select, schema, cursors, arm_end, catalog, &mut sink,
        )?;
        em.place(arm_end);
        Ok(())
    };

    compile_arm(&mut em, &mut reg, 0, first, first_schema)?;
    for (i, (arm_select, arm_schema)) in arm_selects.iter().zip(arm_schemas).enumerate() {
        compile_arm(
            &mut em,
            &mut reg,
            i.saturating_add(1),
            arm_select,
            arm_schema,
        )?;
    }

    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    Ok(em.finish())
}
