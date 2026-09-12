use super::{
    SelectRow,
    expressions::{
        compare_bound_expressions, evaluate_select_expression, infer_query_expression_type,
    },
    grouping::{AggregateOwner, GroupedAggregateValues},
    ordering::{OrderKey, RowOrderSpec, create_order_expression},
    projection::{ProjectionSource, create_projection_expression},
};
use crate::{
    ColumnMeta,
    error::{PgError, Result, SqlState},
    executor::{
        DatabaseState, StatementExecutionContext,
        equality::are_rows_not_distinct,
        expressions::{compare_values, validate_equality_type},
        normalize_identifier,
        scope::BoundScope,
    },
    txn::{Snapshot, Xid},
    value::{BaseType, Value},
};
use sqlparser::ast;
use std::cmp::Ordering;

pub(super) enum DistinctPlan<'a> {
    None,
    Rows,
    On {
        expressions: &'a [ast::Expr],
        keys: Vec<DistinctKey<'a>>,
    },
}

pub(super) enum DistinctKey<'a> {
    Output(usize),
    Order(usize),
    Expression(&'a ast::Expr),
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn resolve_distinct_plan<'a>(
    state: &DatabaseState,
    select: &'a ast::Select,
    projections: &[ProjectionSource<'_>],
    columns: &[ColumnMeta],
    order_specs: &[RowOrderSpec<'_>],
    scope: &BoundScope,
) -> Result<DistinctPlan<'a>> {
    let Some(distinct) = &select.distinct else {
        return Ok(DistinctPlan::None);
    };
    match distinct {
        ast::Distinct::All => Ok(DistinctPlan::None),
        ast::Distinct::Distinct => {
            for projection in projections {
                validate_equality_type(
                    infer_query_expression_type(
                        state,
                        &create_projection_expression(projection, scope),
                        scope,
                    )?
                    .base,
                )?;
            }
            for order in order_specs {
                if matches!(order.key, OrderKey::Output(_)) {
                    continue;
                }
                let order_expression = create_order_expression(order, projections, scope);
                let mut selected = false;
                for projection in projections {
                    if compare_bound_expressions(
                        &order_expression,
                        &create_projection_expression(projection, scope),
                        scope,
                    )? {
                        selected = true;
                        break;
                    }
                }
                if !selected {
                    return Err(PgError::create(
                        SqlState::InvalidColumnReference,
                        "for SELECT DISTINCT, ORDER BY expressions must appear in select list",
                    ));
                }
            }
            Ok(DistinctPlan::Rows)
        }
        ast::Distinct::On(expressions) => {
            if expressions.is_empty() {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "DISTINCT ON requires at least one expression",
                ));
            }
            let output_indexes = expressions
                .iter()
                .map(|expression| {
                    let ast::Expr::Identifier(identifier) = expression else {
                        return None;
                    };
                    columns
                        .iter()
                        .position(|column| column.name == normalize_identifier(identifier))
                })
                .collect::<Vec<_>>();
            for (expression, output_index) in expressions.iter().zip(&output_indexes) {
                let data_type = output_index.map_or_else(
                    || infer_query_expression_type(state, expression, scope).map(|data| data.base),
                    |index| {
                        Ok(BaseType::resolve_oid(columns[index].type_oid)
                            .expect("projection columns use supported PostgreSQL types"))
                    },
                )?;
                validate_equality_type(data_type)?;
            }
            let mut matched = vec![false; expressions.len()];
            for order in order_specs {
                let order_expression = create_order_expression(order, projections, scope);
                let mut found = None;
                for (index, (expression, output_index)) in
                    expressions.iter().zip(&output_indexes).enumerate()
                {
                    let matches = output_index.is_some_and(
                        |output_index| matches!(order.key, OrderKey::Output(index) if index == output_index),
                    ) || output_index.is_none()
                        && compare_bound_expressions(&order_expression, expression, scope)?;
                    if !matched[index] && matches {
                        found = Some(index);
                        break;
                    }
                }
                match found {
                    Some(index) => matched[index] = true,
                    None if matched.iter().all(|matched| *matched) => break,
                    None => {
                        return Err(PgError::create(
                            SqlState::InvalidColumnReference,
                            "SELECT DISTINCT ON expressions must match initial ORDER BY expressions",
                        ));
                    }
                }
            }
            let mut keys = Vec::with_capacity(expressions.len());
            for (expression, output_index) in expressions.iter().zip(output_indexes) {
                let mut key = output_index.map(DistinctKey::Output);
                for (index, projection) in projections.iter().enumerate() {
                    if key.is_some() {
                        break;
                    }
                    if compare_bound_expressions(
                        expression,
                        &create_projection_expression(projection, scope),
                        scope,
                    )? {
                        key = Some(DistinctKey::Output(index));
                        break;
                    }
                }
                if key.is_none() {
                    for (index, order) in order_specs.iter().enumerate() {
                        if compare_bound_expressions(
                            expression,
                            &create_order_expression(order, projections, scope),
                            scope,
                        )? {
                            key = Some(DistinctKey::Order(index));
                            break;
                        }
                    }
                }
                keys.push(key.unwrap_or(DistinctKey::Expression(expression)));
            }
            Ok(DistinctPlan::On { expressions, keys })
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_distinct_keys(
    state: &DatabaseState,
    distinct: &DistinctPlan<'_>,
    values: &[Value],
    order_keys: &[Value],
    scope: &BoundScope,
    row: &[Value],
    aggregate_values: Option<&GroupedAggregateValues>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<Value>> {
    let DistinctPlan::On { keys, .. } = distinct else {
        return Ok(Vec::new());
    };
    keys.iter()
        .enumerate()
        .map(|(index, key)| match key {
            DistinctKey::Output(index) => Ok(values[*index].clone()),
            DistinctKey::Order(index) => Ok(order_keys[*index].clone()),
            DistinctKey::Expression(expression) => evaluate_select_expression(
                state,
                expression,
                scope,
                row,
                aggregate_values.map(|values| (values, AggregateOwner::Distinct(index))),
                xid,
                snapshot,
                context,
            ),
        })
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn remove_duplicate_rows(
    rows: Vec<SelectRow>,
    distinct: &DistinctPlan<'_>,
) -> Result<Vec<SelectRow>> {
    let mut selected: Vec<SelectRow> = Vec::new();
    for row in rows {
        let key = match distinct {
            DistinctPlan::None => {
                selected.push(row);
                continue;
            }
            DistinctPlan::Rows => &row.values,
            DistinctPlan::On { .. } => &row.distinct_keys,
        };
        let mut duplicate = false;
        for existing in &selected {
            let existing_key = match distinct {
                DistinctPlan::Rows => &existing.values,
                DistinctPlan::On { .. } => &existing.distinct_keys,
                DistinctPlan::None => unreachable!("non-distinct rows returned before comparison"),
            };
            if are_rows_not_distinct(existing_key, key)? {
                duplicate = true;
                break;
            }
        }
        if !duplicate {
            selected.push(row);
        }
    }
    Ok(selected)
}

pub(super) fn compare_distinct_keys(left: &SelectRow, right: &SelectRow) -> Ordering {
    left.distinct_keys
        .iter()
        .zip(&right.distinct_keys)
        .find_map(|(left, right)| {
            let ordering = match (left, right) {
                (Value::Null, Value::Null) => Ordering::Equal,
                (Value::Null, _) => Ordering::Greater,
                (_, Value::Null) => Ordering::Less,
                _ => compare_values(left, right).expect("DISTINCT expressions were type-checked"),
            };
            (ordering != Ordering::Equal).then_some(ordering)
        })
        .unwrap_or(Ordering::Equal)
}
