use super::{
    SelectRow,
    expressions::{
        compare_bound_expressions, evaluate_select_expression, infer_query_expression_type,
    },
    grouping::{AggregateOwner, GroupedAggregateValues},
    projection::{ProjectionSource, create_projection_expression},
};
use crate::{
    ColumnMeta,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{
        DatabaseState, StatementExecutionContext,
        expressions::{compare_values, extract_number_literal, validate_ordering_type},
        normalize_identifier, resolve_order_ascending,
        scope::BoundScope,
    },
    txn::{Snapshot, Xid},
    value::{BaseType, Value},
};
use sqlparser::ast;
use std::cmp::Ordering;

pub(super) enum OrderKey<'a> {
    Output(usize),
    Input(usize, &'a ast::Expr),
    Expression(&'a ast::Expr),
}

pub(super) struct RowOrderSpec<'a> {
    pub(super) key: OrderKey<'a>,
    pub(super) ascending: bool,
    pub(super) nulls_first: bool,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn resolve_order_specs<'a>(
    state: &DatabaseState,
    query: &'a ast::Query,
    projections: &[ProjectionSource<'_>],
    columns: &[ColumnMeta],
    scope: &BoundScope,
) -> Result<Vec<RowOrderSpec<'a>>> {
    query
        .order_by
        .as_ref()
        .map(|order_by| {
            if order_by.interpolate.is_some() {
                return reject_unsupported("ORDER BY INTERPOLATE is not implemented");
            }
            let ast::OrderByKind::Expressions(orders) = &order_by.kind else {
                return reject_unsupported("ORDER BY ALL is not implemented");
            };
            orders
                .iter()
                .map(|order| {
                    if order.with_fill.is_some() {
                        return reject_unsupported("ORDER BY WITH FILL is not implemented");
                    }
                    let key = if let Some(position) = extract_number_literal(&order.expr)
                        && !position.contains(['.', 'e', 'E'])
                    {
                        let position = position.parse::<usize>().map_err(|_| {
                            PgError::create(
                                SqlState::InvalidColumnReference,
                                "ORDER BY position is not in select list",
                            )
                        })?;
                        if position == 0 || position > projections.len() {
                            return Err(PgError::create(
                                SqlState::InvalidColumnReference,
                                "ORDER BY position is not in select list",
                            ));
                        }
                        OrderKey::Output(position - 1)
                    } else if let ast::Expr::Identifier(identifier) = &order.expr
                        && let Some(index) = columns
                            .iter()
                            .position(|column| column.name == normalize_identifier(identifier))
                    {
                        OrderKey::Output(index)
                    } else {
                        let mut output = None;
                        for (index, projection) in projections.iter().enumerate() {
                            if compare_bound_expressions(
                                &order.expr,
                                &create_projection_expression(projection, scope),
                                scope,
                            )? {
                                output = Some(index);
                                break;
                            }
                        }
                        match output {
                            Some(index) => OrderKey::Output(index),
                            None => match &order.expr {
                                ast::Expr::Identifier(identifier) => OrderKey::Input(
                                    scope.resolve_column(std::slice::from_ref(identifier))?.0,
                                    &order.expr,
                                ),
                                ast::Expr::CompoundIdentifier(identifiers) => OrderKey::Input(
                                    scope.resolve_column(identifiers)?.0,
                                    &order.expr,
                                ),
                                _ => {
                                    infer_query_expression_type(state, &order.expr, scope)?;
                                    OrderKey::Expression(&order.expr)
                                }
                            },
                        }
                    };
                    let data_type = match &key {
                        OrderKey::Output(index) => BaseType::resolve_oid(columns[*index].type_oid)
                            .expect("projection columns use supported PostgreSQL types"),
                        OrderKey::Input(slot, _) => {
                            scope
                                .columns
                                .iter()
                                .find(|column| column.slot == *slot)
                                .expect("resolved ORDER BY column is in scope")
                                .data_type
                                .base
                        }
                        OrderKey::Expression(expression) => {
                            infer_query_expression_type(state, expression, scope)?.base
                        }
                    };
                    validate_ordering_type(data_type)?;
                    let ascending = resolve_order_ascending(&order.options)?;
                    Ok(RowOrderSpec {
                        key,
                        ascending,
                        nulls_first: order.options.nulls_first.unwrap_or(!ascending),
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()
        .map(|orders| orders.unwrap_or_default())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn create_order_expression(
    order: &RowOrderSpec<'_>,
    projections: &[ProjectionSource<'_>],
    scope: &BoundScope,
) -> ast::Expr {
    match order.key {
        OrderKey::Output(index) => create_projection_expression(&projections[index], scope),
        OrderKey::Input(_, expression) => expression.clone(),
        OrderKey::Expression(expression) => expression.clone(),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_order_keys(
    state: &DatabaseState,
    order_specs: &[RowOrderSpec<'_>],
    values: &[Value],
    scope: &BoundScope,
    row: &[Value],
    aggregate_values: Option<&GroupedAggregateValues>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<Value>> {
    order_specs
        .iter()
        .enumerate()
        .map(|(index, order)| match order.key {
            OrderKey::Output(index) => Ok(values[index].clone()),
            OrderKey::Input(slot, _) => Ok(row[slot].clone()),
            OrderKey::Expression(expression) => evaluate_select_expression(
                state,
                expression,
                scope,
                row,
                aggregate_values.map(|values| (values, AggregateOwner::Order(index))),
                xid,
                snapshot,
                context,
            ),
        })
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn sort_ordered_rows(rows: &mut [SelectRow], order_specs: &[RowOrderSpec<'_>]) {
    if !order_specs.is_empty() {
        rows.sort_by(|left, right| compare_ordered_rows(left, right, order_specs));
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn compare_ordered_rows(
    left: &SelectRow,
    right: &SelectRow,
    order_specs: &[RowOrderSpec<'_>],
) -> Ordering {
    compare_order_keys(&left.keys, &right.keys, order_specs)
}

pub(super) fn compare_order_keys(
    left: &[Value],
    right: &[Value],
    order_specs: &[RowOrderSpec<'_>],
) -> Ordering {
    order_specs
        .iter()
        .zip(left.iter().zip(right))
        .find_map(|(spec, (left, right))| {
            let ordering = match (left, right) {
                (Value::Null, Value::Null) => Ordering::Equal,
                (Value::Null, _) => {
                    if spec.nulls_first {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (_, Value::Null) => {
                    if spec.nulls_first {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                _ => {
                    let ordering =
                        compare_values(left, right).expect("ORDER BY expression type was checked");
                    if spec.ascending {
                        ordering
                    } else {
                        ordering.reverse()
                    }
                }
            };
            (ordering != Ordering::Equal).then_some(ordering)
        })
        .unwrap_or(Ordering::Equal)
}

pub(super) fn retain_top_ordered_row(
    rows: &mut Vec<SelectRow>,
    row: SelectRow,
    top_k: Option<usize>,
    order_specs: &[RowOrderSpec<'_>],
) {
    let Some(top_k) = top_k else {
        rows.push(row);
        return;
    };
    if top_k == 0 {
        return;
    }
    if rows.len() < top_k {
        rows.push(row);
        let mut child = rows.len() - 1;
        while child > 0 {
            let parent = (child - 1) / 2;
            if compare_ordered_rows(&rows[parent], &rows[child], order_specs) != Ordering::Less {
                break;
            }
            rows.swap(parent, child);
            child = parent;
        }
        return;
    }
    if compare_ordered_rows(&row, &rows[0], order_specs) != Ordering::Less {
        return;
    }
    rows[0] = row;
    let mut parent = 0;
    loop {
        let left = parent * 2 + 1;
        if left >= rows.len() {
            break;
        }
        let right = left + 1;
        let worse_child = if right < rows.len()
            && compare_ordered_rows(&rows[left], &rows[right], order_specs) == Ordering::Less
        {
            right
        } else {
            left
        };
        if compare_ordered_rows(&rows[parent], &rows[worse_child], order_specs) != Ordering::Less {
            break;
        }
        rows.swap(parent, worse_child);
        parent = worse_child;
    }
}
