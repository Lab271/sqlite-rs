//! Statement dispatch: keyword-sniffs a raw SQL string to pick the right
//! parser/compiler pair for one INSERT/UPDATE/DELETE/CREATE TABLE/CREATE
//! INDEX/DROP TABLE/DROP INDEX statement (#292 — moved out of the CLI
//! binary so it's usable without depending on the binary crate, e.g. by
//! a future REPL).

use thiserror::Error;

use crate::parser::ast::{InsertSource, TableRefKind};
use crate::parser::error::ParseOutcome;
use crate::parser::error::{
    parse_begin, parse_commit, parse_create_index, parse_create_table, parse_create_view,
    parse_delete, parse_drop_index, parse_drop_table, parse_insert, parse_rollback, parse_update,
};
use crate::schema::{TableSchema, ViewSchema};
use crate::vdbe::Program;

use super::{
    compile_begin, compile_commit, compile_create_index, compile_create_table, compile_create_view,
    compile_delete_with_catalog, compile_drop_index, compile_drop_table, compile_insert,
    compile_rollback, compile_update_with_catalog, expand_views, expand_with_clause,
    resolve_from_table_schema, resolve_views, CodegenError,
};

/// Failure compiling one dispatched statement — everything
/// [`compile_statement`] can fail with, folded into one error type so
/// callers (the CLI, a future REPL) don't need to know about the
/// per-statement parser/codegen error types individually.
#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("no such table: {0}")]
    NoSuchTable(String),

    #[error("no such index: {0}")]
    NoSuchIndex(String),

    #[error("unsupported or unrecognized statement: {0:?} ...")]
    Unrecognized(String),

    #[error("SELECT has no FROM clause")]
    NoFromClause,

    #[error(transparent)]
    Codegen(#[from] CodegenError),

    #[error("{0}")]
    ParseFailed(String),
}

/// The first one or two whitespace-separated words of `sql`, uppercased
/// — enough to pick which statement-specific parser to hand `sql` to
/// (`CREATE TABLE` vs `CREATE INDEX`/`CREATE UNIQUE INDEX`, `DROP TABLE`
/// vs `DROP INDEX`), without re-tokenizing the whole statement twice.
pub fn leading_keywords(sql: &str) -> Vec<String> {
    sql.split_whitespace()
        .take(3)
        .map(|w| w.to_ascii_uppercase())
        .collect()
}

fn parse_error<T: std::fmt::Debug>(other: ParseOutcome<T>) -> DispatchError {
    DispatchError::ParseFailed(format!("{other:?}"))
}

/// Parses `sql`, picks the compiler for its leading keyword(s), and
/// compiles it against `schemas` — the `exec <file> "<SQL>"` CLI
/// subcommand's core (#215's write-path CLI surface), shared by any
/// future caller that needs to run a single INSERT/UPDATE/DELETE/CREATE
/// TABLE/CREATE INDEX/DROP TABLE/DROP INDEX statement against a known
/// catalog.
pub fn compile_statement(
    sql: &str,
    schemas: &[TableSchema],
    views: &[ViewSchema],
) -> Result<Program, DispatchError> {
    let find_schema = |name: &str| -> Result<&TableSchema, DispatchError> {
        schemas
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| DispatchError::NoSuchTable(name.to_string()))
    };
    let find_index_root = |name: &str| -> Result<u32, DispatchError> {
        schemas
            .iter()
            .flat_map(|s| &s.indexes)
            .find(|idx| idx.name.eq_ignore_ascii_case(name))
            .map(|idx| idx.root_page)
            .ok_or_else(|| DispatchError::NoSuchIndex(name.to_string()))
    };

    let keywords = leading_keywords(sql);
    let kw = |i: usize| keywords.get(i).map(String::as_str).unwrap_or("");

    match kw(0) {
        "BEGIN" => match parse_begin(sql) {
            ParseOutcome::Accepted(begin) => Ok(compile_begin(&begin)),
            other => Err(parse_error(other)),
        },
        "COMMIT" | "END" => match parse_commit(sql) {
            ParseOutcome::Accepted(commit) => Ok(compile_commit(&commit)),
            other => Err(parse_error(other)),
        },
        "ROLLBACK" => match parse_rollback(sql) {
            ParseOutcome::Accepted(rollback) => Ok(compile_rollback(&rollback)),
            other => Err(parse_error(other)),
        },
        "INSERT" => match parse_insert(sql) {
            ParseOutcome::Accepted(mut insert) => {
                let schema = find_schema(&insert.table)?;
                let select_schemas: Option<Vec<TableSchema>> = match &insert.source {
                    InsertSource::Select(select) => {
                        // Same `WITH`/view expansion `compile_select_program`
                        // runs for a plain SELECT (#375/#380), so a CTE/view
                        // name in the source at least *resolves* against the
                        // catalog instead of failing with an unexplained "no
                        // such table". The INSERT codegen path below
                        // (`compile_insert`'s single-/joined-table scan)
                        // only knows how to scan a *real* table's root page,
                        // though — it doesn't yet drive `#257`'s FROM-
                        // subquery materialization the way a plain SELECT's
                        // codegen does — so a CTE/view expanding into a
                        // `TableRefKind::Subquery` here is rejected
                        // explicitly rather than falling through to
                        // `compile_insert` and failing with a confusing
                        // "invalid root page (0)" (the subquery's synthetic,
                        // rootpage-less schema).
                        let resolved_views = resolve_views(views);
                        let cte_expanded = expand_with_clause(select);
                        let expanded = expand_views(&cte_expanded, &resolved_views)?;

                        let Some(from) = &expanded.from else {
                            return Err(DispatchError::NoFromClause);
                        };
                        let is_subquery = |r: &crate::parser::ast::TableRef| {
                            matches!(r.kind, TableRefKind::Subquery(_))
                        };
                        if is_subquery(&from.first)
                            || from.joins.iter().any(|j| is_subquery(&j.table))
                        {
                            return Err(CodegenError::Unsupported {
                                reason: "INSERT ... SELECT with a CTE or view source is not yet \
                                         supported"
                                    .to_string(),
                            }
                            .into());
                        }
                        let mut joined_schemas =
                            vec![resolve_from_table_schema(&from.first, schemas)?];
                        for join in &from.joins {
                            joined_schemas.push(resolve_from_table_schema(&join.table, schemas)?);
                        }
                        insert.source = InsertSource::Select(Box::new(expanded));
                        Some(joined_schemas)
                    }
                    InsertSource::Values(_) | InsertSource::DefaultValues => None,
                };
                Ok(compile_insert(&insert, schema, select_schemas.as_deref())?)
            }
            other => Err(parse_error(other)),
        },
        "UPDATE" => match parse_update(sql) {
            ParseOutcome::Accepted(update) => {
                let schema = find_schema(&update.table)?;
                Ok(compile_update_with_catalog(&update, schema, schemas)?)
            }
            other => Err(parse_error(other)),
        },
        "DELETE" => match parse_delete(sql) {
            ParseOutcome::Accepted(delete) => {
                let schema = find_schema(&delete.table)?;
                Ok(compile_delete_with_catalog(&delete, schema, schemas)?)
            }
            other => Err(parse_error(other)),
        },
        "CREATE" if kw(1) == "TABLE" => match parse_create_table(sql) {
            ParseOutcome::Accepted(create) => Ok(compile_create_table(&create, sql)?),
            other => Err(parse_error(other)),
        },
        "CREATE" if kw(1) == "VIEW" => match parse_create_view(sql) {
            ParseOutcome::Accepted(create) => Ok(compile_create_view(&create, sql)?),
            other => Err(parse_error(other)),
        },
        "CREATE" if kw(1) == "INDEX" || kw(1) == "UNIQUE" => match parse_create_index(sql) {
            ParseOutcome::Accepted(ci) => {
                let schema = find_schema(&ci.table)?;
                Ok(compile_create_index(&ci, schema, sql)?)
            }
            other => Err(parse_error(other)),
        },
        "DROP" if kw(1) == "TABLE" => match parse_drop_table(sql) {
            ParseOutcome::Accepted(drop) => {
                let schema = find_schema(&drop.name)?;
                Ok(compile_drop_table(&drop, schema)?)
            }
            other => Err(parse_error(other)),
        },
        "DROP" if kw(1) == "INDEX" => match parse_drop_index(sql) {
            ParseOutcome::Accepted(di) => {
                let root_page = find_index_root(&di.name)?;
                Ok(compile_drop_index(&di, root_page)?)
            }
            other => Err(parse_error(other)),
        },
        other => Err(DispatchError::Unrecognized(other.to_string())),
    }
}
