//! `FROM`-subquery schema resolution and materialization — see
//! `super`'s module doc.

use crate::codegen::select::{
    compile_select_joined_scan, compile_select_scan, CodegenError, ScanCursors,
};
use crate::codegen::{Emitter, RegAlloc};
use crate::parser::ast::{ExprKind, ResultColumn, Select, TableRef};
use crate::schema::TableSchema;
use crate::vdbe::{Instruction, Opcode, P4};

/// Resolves a subquery's own single-table `FROM` against `catalog`,
/// rejecting anything this MVP pass doesn't materialize: no `FROM` at
/// all is only valid when the subquery has no column references (e.g.
/// `SELECT (SELECT 1)`), and a `JOIN`ed `FROM` isn't supported.
pub(crate) fn resolve_subquery_schema(
    subselect: &Select,
    catalog: &[TableSchema],
) -> Result<Option<TableSchema>, CodegenError> {
    let Some(from) = &subselect.from else {
        return Ok(None);
    };
    if !from.joins.is_empty() {
        return Err(CodegenError::Unsupported {
            reason: "a subquery whose own FROM clause has a JOIN is not yet supported".to_string(),
        });
    }
    let Some(name) = from.first.name() else {
        return Err(CodegenError::Unsupported {
            reason: "a subquery-expression's own FROM being itself a subquery is not yet \
                     supported"
                .to_string(),
        });
    };
    let schema = catalog
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .cloned()
        .ok_or_else(|| CodegenError::Unsupported {
            reason: format!(
                "subquery references table {name:?}, which isn't visible to this compiler's \
                 catalog"
            ),
        })?;
    Ok(Some(schema))
}

/// The column names a `FROM`-subquery's own `SELECT` list exposes to the
/// enclosing query (#257) — used to build the synthetic [`TableSchema`]
/// a materialized subquery-in-FROM is bound into `Scope` as.
/// `table_refs`/`schemas` are the subquery's own resolved `FROM` tables,
/// same order, for `*`/`table.*` expansion; an unaliased computed
/// expression falls back to a positional `columnN` name (`N` 1-based),
/// same convention SQLite itself uses for an anonymous result column.
fn subquery_output_columns(
    subquery: &Select,
    table_refs: &[&TableRef],
    schemas: &[TableSchema],
) -> Vec<String> {
    let mut out = Vec::new();
    for (i, col) in subquery.columns.iter().enumerate() {
        match col {
            ResultColumn::Star => {
                for schema in schemas {
                    out.extend(schema.columns.iter().cloned());
                }
            }
            ResultColumn::TableStar { table } => {
                if let Some(schema) = table_refs
                    .iter()
                    .position(|t| t.alias.as_deref().or(t.name()).unwrap_or("") == table)
                    .and_then(|idx| schemas.get(idx))
                {
                    out.extend(schema.columns.iter().cloned());
                }
            }
            ResultColumn::Expr { expr, alias } => {
                let name = alias.clone().unwrap_or_else(|| match &expr.kind {
                    ExprKind::Column { name, .. } => name.clone(),
                    _ => format!("column{}", i.saturating_add(1)),
                });
                out.push(name);
            }
        }
    }
    out
}

/// A `FROM`-subquery's own `FROM` table(s) (#257) — the first table plus
/// every join's table, same order. Split from [`resolve_subquery_schemas`]
/// (rather than returning both together) because a function borrowing
/// from two different reference parameters (`subquery` here, `catalog`
/// there) can't have its output lifetime elided, and this codebase's
/// `make mvl-limit` gate forbids writing an explicit lifetime to spell
/// it out.
pub(super) fn subquery_own_table_refs(subquery: &Select) -> Result<Vec<&TableRef>, CodegenError> {
    let Some(from) = &subquery.from else {
        return Err(CodegenError::Unsupported {
            reason: "a subquery in FROM must itself have a FROM clause".to_string(),
        });
    };
    Ok(std::iter::once(&from.first)
        .chain(from.joins.iter().map(|j| &j.table))
        .collect())
}

/// Resolves each of `table_refs` against `catalog` — one schema per
/// table, same order. A subquery nested inside another subquery's
/// `FROM` is not yet supported (this pass materializes one level).
fn resolve_subquery_schemas(
    table_refs: &[&TableRef],
    catalog: &[TableSchema],
) -> Result<Vec<TableSchema>, CodegenError> {
    table_refs
        .iter()
        .map(|table_ref| {
            let Some(name) = table_ref.name() else {
                return Err(CodegenError::Unsupported {
                    reason: "a subquery nested inside another subquery's FROM is not yet \
                             supported"
                        .to_string(),
                });
            };
            catalog
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(name))
                .cloned()
                .ok_or_else(|| CodegenError::Unsupported {
                    reason: format!("no such table: {name}"),
                })
        })
        .collect()
}

/// Builds the synthetic [`TableSchema`] (#257) a materialized
/// subquery-in-FROM is bound into `Scope` as — `name` left empty, since
/// only the caller (which has the `TableRef`) knows the subquery's
/// mandatory alias.
fn subquery_result_schema(
    subquery: &Select,
    table_refs: &[&TableRef],
    schemas: &[TableSchema],
) -> TableSchema {
    let columns = subquery_output_columns(subquery, table_refs, schemas);
    TableSchema {
        name: String::new(),
        root_page: 0,
        columns: columns.clone(),
        without_rowid: false,
        strict: false,
        column_types: vec![String::new(); columns.len()],
        is_virtual: false,
        sql: String::new(),
        indexes: Vec::new(),
    }
}

/// Resolves `table_ref` to the [`TableSchema`] the rest of codegen
/// should treat it as: a real catalog lookup by name, or (#257) the
/// synthetic schema describing a `FROM`-subquery's own projected
/// columns (its `name` is `table_ref`'s alias — mandatory for a
/// subquery, enforced by the parser). Used by callers (the `sqlite-rs`
/// CLI, `INSERT ... SELECT`) that need a `TableSchema` up front, before
/// the codegen pass that actually emits the materialization
/// (`compile_select_with_catalog`/`compile_select_joined` call
/// [`materialize_from_subquery`] themselves once compiling).
pub fn resolve_from_table_schema(
    table_ref: &TableRef,
    catalog: &[TableSchema],
) -> Result<TableSchema, CodegenError> {
    match &table_ref.kind {
        crate::parser::ast::TableRefKind::Name(name) => catalog
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or_else(|| CodegenError::Unsupported {
                reason: format!("no such table: {name}"),
            }),
        crate::parser::ast::TableRefKind::Subquery(subquery) => {
            let table_refs = subquery_own_table_refs(subquery)?;
            let schemas = resolve_subquery_schemas(&table_refs, catalog)?;
            let mut schema = subquery_result_schema(subquery, &table_refs, &schemas);
            schema.name = table_ref.alias.clone().unwrap_or_default();
            Ok(schema)
        }
    }
}

/// Materializes a `FROM`-subquery (#257) into an in-memory ephemeral
/// table opened on `dest_cursor`, so the enclosing query can then scan
/// it exactly like a real table cursor (`Rewind`/`Next`/`Column`/
/// `Rowid`). Drives the subquery's own scan through
/// [`compile_select_scan`] (single-table) or [`compile_select_joined_scan`]
/// (its own `FROM` has a JOIN — criterion 3), substituting a row sink
/// that `MakeRecord`s each projected row and `Insert`s it into
/// `dest_cursor` with a freshly `Sequence`d rowid, in place of
/// `ResultRow` — the same substitution #208's `INSERT ... SELECT`
/// codegen uses. Returns the synthetic [`TableSchema`] (`name` left
/// empty — the caller fills in the subquery's alias) describing the
/// materialized table's columns, for the caller to bind into `Scope`.
/// A subquery nested inside another subquery's `FROM` is not yet
/// supported (this pass materializes one level).
pub(crate) fn materialize_from_subquery(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    subquery: &Select,
    catalog: &[TableSchema],
    dest_cursor: i32,
) -> Result<TableSchema, CodegenError> {
    let table_refs = subquery_own_table_refs(subquery)?;
    let schemas = resolve_subquery_schemas(&table_refs, catalog)?;
    let Some(from) = &subquery.from else {
        return Err(CodegenError::Unsupported {
            reason: "a subquery in FROM must itself have a FROM clause".to_string(),
        });
    };
    let synthetic_schema = subquery_result_schema(subquery, &table_refs, &schemas);

    em.emit(Instruction {
        opcode: Opcode::OpenEphemeral,
        p1: dest_cursor,
        p2: 0,
        p3: 0,
        p4: P4::None,
        p5: 1,
    });

    let end_label = em.new_label();
    let mut sink = |em: &mut Emitter, reg: &mut RegAlloc, first: i32, count: i32| {
        let rowid_reg = reg.alloc();
        em.emit(Instruction::new(
            Opcode::Sequence,
            dest_cursor,
            rowid_reg,
            0,
        ));
        let record_reg = reg.alloc();
        em.emit(Instruction::new(
            Opcode::MakeRecord,
            first,
            count,
            record_reg,
        ));
        em.emit(Instruction::new(
            Opcode::Insert,
            dest_cursor,
            rowid_reg,
            record_reg,
        ));
        Ok(())
    };

    if from.joins.is_empty() {
        let schema = schemas.first().ok_or_else(|| CodegenError::Unsupported {
            reason: "materialized subquery FROM has no schema".to_string(),
        })?;
        let cursors = ScanCursors {
            table: reg.alloc_cursor(),
            sort: reg.alloc_cursor(),
            pseudo: reg.alloc_cursor(),
            distinct: reg.alloc_cursor(),
        };
        em.emit(Instruction::new(
            Opcode::OpenRead,
            cursors.table,
            i32::try_from(schema.root_page).unwrap_or(0),
            0,
        ));
        compile_select_scan(
            em, reg, subquery, schema, cursors, end_label, catalog, &mut sink,
        )?;
    } else {
        let cursor_base = reg.alloc_cursor();
        // Reserve `table_count + 2` contiguous cursor numbers (one per
        // joined table, plus the sort/pseudo or distinct cursor
        // `compile_select_joined_scan` may itself derive by offsetting
        // from `cursor_base`) so a later `reg.alloc_cursor()` call (e.g.
        // for a correlated subquery expression inside this subquery)
        // can't collide with a number that function computes by
        // arithmetic rather than by calling `alloc_cursor` itself.
        for _ in 0..schemas.len().saturating_add(1) {
            reg.alloc_cursor();
        }
        compile_select_joined_scan(
            em,
            reg,
            subquery,
            &schemas,
            catalog,
            cursor_base,
            end_label,
            &mut sink,
        )?;
    }
    em.place(end_label);

    Ok(synthetic_schema)
}
