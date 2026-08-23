//! `CREATE VIEW` query expansion (#380) — the catalog counterpart to
//! [`super::cte::expand_with_clause`]: every `FROM`/`JOIN` table
//! reference naming a catalog view becomes a `TableRefKind::Subquery`
//! wrapping that view's stored `Select`, reusing exactly the same
//! `TableRefKind::Subquery` materialization path (#257) CTEs already
//! ride on. Unlike CTEs (declared inline, resolved in one pass in
//! declaration order), every view is already fully defined in the
//! catalog before a query ever runs, so a view referencing another view
//! (nested views) is resolved by recursing into the substituted
//! subquery's own `FROM`/`JOIN` clauses with the same view list. Since
//! `CREATE VIEW` never checks for cycles at DDL time, a view can
//! reference itself directly or transitively (through other views); the
//! stack of view names currently being expanded is tracked so such a
//! cycle is rejected with [`CodegenError::CircularView`] (matching stock
//! SQLite's own "view X is circularly defined" wording) instead of
//! recursing forever.

use crate::parser::ast::{Select, TableRef, TableRefKind};
use crate::parser::error::ParseOutcome;

use crate::codegen::select::CodegenError;

use super::cte::{apply_column_aliases, expand_with_clause};

/// A view's catalog entry, pre-parsed once per query by the caller
/// (`bin/sqlite-rs/query.rs`) from `schema::ViewSchema::sql`.
pub struct ResolvedView {
    pub name: String,
    pub columns: Option<Vec<String>>,
    pub query: Box<Select>,
}

/// Parses every `schema::ViewSchema` into a [`ResolvedView`], silently
/// dropping any view whose stored `sql` no longer parses (should not
/// happen in practice — `sqlite_master.sql` is only ever written by
/// [`crate::codegen::compile_create_view`] itself — but this module
/// follows the same graceful-degradation convention as
/// `schema::ddl_reader`'s unparseable-DDL handling rather than turning a
/// single bad row into a hard failure for every other query).
pub fn resolve_views(views: &[crate::schema::ViewSchema]) -> Vec<ResolvedView> {
    views
        .iter()
        .filter_map(|v| match crate::parser::parse_create_view(&v.sql) {
            ParseOutcome::Accepted(create) => Some(ResolvedView {
                name: create.name,
                columns: create.columns,
                query: create.query,
            }),
            _ => None,
        })
        .collect()
}

/// Rewrites away every catalog-view reference in `select`'s `FROM`/
/// `JOIN` clauses (main query and each `UNION`/`UNION ALL` arm),
/// recursively. A `Select` that references no view is returned
/// unchanged (cloned, matching [`super::cte::expand_with_clause`]'s
/// contract of always handing back an owned value). Fails with
/// [`CodegenError::CircularView`] if a view directly or transitively
/// references itself.
pub fn expand_views(select: &Select, views: &[ResolvedView]) -> Result<Select, CodegenError> {
    let mut out = select.clone();
    let mut stack = Vec::new();
    expand_views_in_select(&mut out, views, &mut stack)?;
    Ok(out)
}

fn expand_views_in_select(
    select: &mut Select,
    views: &[ResolvedView],
    stack: &mut Vec<String>,
) -> Result<(), CodegenError> {
    if let Some(from) = &mut select.from {
        expand_table_ref(&mut from.first, views, stack)?;
        for join in &mut from.joins {
            expand_table_ref(&mut join.table, views, stack)?;
        }
    }
    for arm in &mut select.compound {
        if let Some(from) = &mut arm.from {
            expand_table_ref(&mut from.first, views, stack)?;
            for join in &mut from.joins {
                expand_table_ref(&mut join.table, views, stack)?;
            }
        }
    }
    Ok(())
}

fn expand_table_ref(
    table_ref: &mut TableRef,
    views: &[ResolvedView],
    stack: &mut Vec<String>,
) -> Result<(), CodegenError> {
    match &mut table_ref.kind {
        TableRefKind::Name(name) => {
            let Some(view) = views.iter().find(|v| v.name.eq_ignore_ascii_case(name)) else {
                return Ok(());
            };
            if stack
                .iter()
                .any(|seen| seen.eq_ignore_ascii_case(&view.name))
            {
                return Err(CodegenError::CircularView {
                    name: view.name.clone(),
                });
            }
            // A view's own stored body may carry its own `WITH` clause
            // (`CREATE VIEW v AS WITH cte AS (...) SELECT * FROM cte`) —
            // that never runs through `expand_with_clause` otherwise,
            // since only the outermost query gets that pass at the top
            // of `compile_select_program`. Expanding it here, before
            // recursing for nested views, mirrors the ordering already
            // used at the top level (CTEs first, then views).
            let mut query = expand_with_clause(&view.query);
            if let Some(columns) = &view.columns {
                apply_column_aliases(&mut query, columns);
            }
            stack.push(view.name.clone());
            let result = expand_views_in_select(&mut query, views, stack);
            stack.pop();
            result?;
            // Same alias-defaulting rule as CTE substitution (#376): a
            // bare `FROM view_name` (no explicit alias) needs the
            // subquery's alias set to the view's own name, since a
            // `TableRefKind::Subquery`'s alias is mandatory to the rest
            // of codegen (`resolve_from_table_schema`'s doc comment).
            let alias = table_ref.alias.clone().or_else(|| Some(view.name.clone()));
            table_ref.kind = TableRefKind::Subquery(Box::new(query));
            table_ref.alias = alias;
            Ok(())
        }
        TableRefKind::Subquery(inner) => expand_views_in_select(inner, views, stack),
    }
}
