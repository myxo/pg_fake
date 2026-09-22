use crate::executor::{
    DatabaseState, StatementContext,
    column_defaults::is_default_expression,
    expressions::{extract_unknown_string_literal, is_null_literal},
    normalize_unqualified_object_name, prepared, query,
    scope::BoundScope,
    subqueries,
};
use crate::{
    catalog::{RelationName, TableSchema},
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    txn::{Snapshot, Xid},
    value::{BaseType, PgType, Value},
};
use sqlparser::ast;
use std::collections::BTreeSet;

mod conflicts;
mod delete;
mod insert;
mod insert_preparation;
mod resume;
mod returning;
pub(crate) use resume::PreparedWrite;
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
    subscript: Option<&'a ast::Expr>,
    prepared: Option<prepared::PreparedExpression>,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_mutation_assignment(
    state: &DatabaseState,
    assignment: &MutationAssignment<'_>,
    target: PgType,
    current: &Value,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Value> {
    let value_target = assignment
        .subscript
        .map(|_| {
            PgType::create_with_typmod(
                target
                    .base
                    .get_array_element_type()
                    .expect("subscript assignment targets an array"),
                target.typmod,
            )
        })
        .unwrap_or(target);
    let assigned = if let Some(text) = extract_unknown_string_literal(assignment.expression) {
        crate::executor::expressions::coerce_unknown_with_context(
            text,
            value_target,
            CastContext::Assignment,
            context,
        )
    } else {
        coercion::coerce(
            subqueries::evaluate_query_expression(
                state,
                assignment.expression,
                scope,
                row,
                xid,
                snapshot,
                context,
            )?,
            query::infer_query_expression_type(state, assignment.expression, scope)?.base,
            value_target,
            CastContext::Assignment,
            &context.get_timezone(),
        )
    }?;
    let Some(subscript) = assignment.subscript else {
        return Ok(assigned);
    };
    let index = subqueries::evaluate_query_expression(
        state, subscript, scope, row, xid, snapshot, context,
    )?;
    let index_source = query::infer_query_expression_type(state, subscript, scope)?.base;
    let index = coercion::coerce(
        index,
        index_source,
        PgType::create(BaseType::Int4),
        CastContext::Assignment,
        &context.get_timezone(),
    )?;
    let Value::Int4(index) = index else {
        return Err(PgError::create(
            SqlState::NullValueNotAllowed,
            "array subscript in assignment must not be null",
        ));
    };
    let index = usize::try_from(index.checked_sub(1).ok_or_else(|| {
        PgError::create(
            SqlState::ArraySubscriptError,
            "array subscript is out of range",
        )
    })?)
    .map_err(|_| {
        PgError::create(
            SqlState::ArraySubscriptError,
            "array subscript is out of range",
        )
    })?;
    let mut values = match current {
        Value::Array { values, .. } => values.clone(),
        Value::Null => Vec::new(),
        _ => unreachable!("subscript assignment target contains an array"),
    };
    if values.is_empty() && index > 0 {
        return reject_unsupported(
            "array assignment creating a non-default lower bound is not implemented",
        );
    }
    if values.len() <= index {
        values.resize(index + 1, Value::Null);
    }
    values[index] = assigned;
    Ok(Value::Array {
        elem_type: value_target.base,
        values,
    })
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
            let (column, subscript) = match &assignment.target {
                ast::AssignmentTarget::ColumnName(column) => (column, None),
                ast::AssignmentTarget::Subscript { column, subscripts } => {
                    let [ast::Subscript::Index { index }] = subscripts.as_slice() else {
                        return reject_unsupported("array assignment shape is not implemented");
                    };
                    (column, Some(index))
                }
                ast::AssignmentTarget::Tuple(_) => {
                    return reject_unsupported("UPDATE tuple assignment is not implemented");
                }
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
            let target = if subscript.is_some() {
                schema.columns[index]
                    .data_type
                    .base
                    .get_array_element_type()
                    .ok_or_else(|| {
                        PgError::create(
                            SqlState::DatatypeMismatch,
                            "subscripted assignment target is not an array",
                        )
                    })?
            } else {
                schema.columns[index].data_type.base
            };
            if subscript.is_some() && is_default_expression(&assignment.value) {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "cannot assign DEFAULT to an array element",
                ));
            }
            if !is_default_expression(&assignment.value)
                && !is_null_literal(&assignment.value)
                && extract_unknown_string_literal(&assignment.value).is_none()
                && !coercion::can_cast(
                    query::infer_query_expression_type(state, &assignment.value, scope)?.base,
                    target,
                    CastContext::Assignment,
                )
            {
                return Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "column has incompatible type",
                ));
            }
            let prepared = if is_default_expression(&assignment.value) || subscript.is_some() {
                None
            } else {
                prepared::bind_prepared_expression(&assignment.value, scope, &[])?
                    .filter(|expression| expression.get_data_type() == target)
            };
            Ok(MutationAssignment {
                index,
                expression: &assignment.value,
                subscript,
                prepared,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((assigned, assignments))
}
