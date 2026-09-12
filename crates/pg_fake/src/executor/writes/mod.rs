use crate::executor::{
    DatabaseState, StatementContext,
    expressions::{extract_unknown_string_literal, is_default_expression, is_null_literal},
    normalize_unqualified_object_name, prepared, query,
    scope::BoundScope,
    subqueries,
};
use crate::{
    catalog::{RelationName, TableSchema},
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    txn::{Snapshot, Xid},
    value::{PgType, Value},
};
use sqlparser::ast;
use std::collections::BTreeSet;

mod conflicts;
mod delete;
mod insert;
mod insert_preparation;
mod returning;
mod targets;
mod update;

pub(super) use conflicts::resolve_conflict_arbiter;
pub(super) use delete::execute_delete;
pub(super) use insert::{execute_insert, resolve_insert_column_indexes};
pub(super) use insert_preparation::prepare_insert_rows;
pub(super) use targets::{
    collect_delete_cte_locks, collect_update_cte_locks, create_mutation_scope,
};
pub(super) use update::{execute_update, prepare_update_rows};

fn require_mutation_table(state: &DatabaseState, name: &RelationName) -> Result<TableSchema> {
    if state.catalog.require_named_view(name).is_ok() {
        return reject_unsupported("mutations targeting views are not implemented");
    }
    Ok(state.catalog.require_named_table(name)?.clone())
}

struct MutationAssignment<'a> {
    index: usize,
    expression: &'a ast::Expr,
    prepared: Option<prepared::PreparedExpression>,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_mutation_assignment(
    state: &DatabaseState,
    expression: &ast::Expr,
    target: PgType,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Value> {
    if let Some(text) = extract_unknown_string_literal(expression) {
        coercion::coerce_unknown(text, target, CastContext::Assignment)
    } else {
        coercion::coerce(
            subqueries::evaluate_query_expression(
                state, expression, scope, row, xid, snapshot, context,
            )?,
            query::infer_query_expression_type(state, expression, scope)?.base,
            target,
            CastContext::Assignment,
        )
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn build_mutation_assignments<'a>(
    state: &DatabaseState,
    schema: &TableSchema,
    scope: &BoundScope,
    assignments: &'a [ast::Assignment],
) -> Result<(BTreeSet<usize>, Vec<MutationAssignment<'a>>)> {
    let mut assigned = BTreeSet::new();
    let assignments = assignments
        .iter()
        .map(|assignment| {
            let ast::AssignmentTarget::ColumnName(column) = &assignment.target else {
                return reject_unsupported("UPDATE tuple assignment is not implemented");
            };
            let column_name = normalize_unqualified_object_name(column)?;
            let index = schema
                .columns
                .iter()
                .position(|definition| definition.name == column_name)
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::UndefinedColumn,
                        format!("column {column_name:?} does not exist"),
                    )
                })?;
            if !assigned.insert(index) {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "multiple assignments to the same column",
                ));
            }
            if !is_default_expression(&assignment.value)
                && !is_null_literal(&assignment.value)
                && extract_unknown_string_literal(&assignment.value).is_none()
                && !coercion::can_cast(
                    query::infer_query_expression_type(state, &assignment.value, scope)?.base,
                    schema.columns[index].data_type.base,
                    CastContext::Assignment,
                )
            {
                return Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "column has incompatible type",
                ));
            }
            let prepared = if is_default_expression(&assignment.value) {
                None
            } else {
                prepared::bind_prepared_expression(&assignment.value, scope, &[])?.filter(
                    |expression| expression.get_data_type() == schema.columns[index].data_type.base,
                )
            };
            Ok(MutationAssignment {
                index,
                expression: &assignment.value,
                prepared,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((assigned, assignments))
}
