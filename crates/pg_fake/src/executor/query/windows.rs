use sqlparser::ast::{self, VisitMut as _};
use std::cmp::Ordering;

use crate::{
    coercion::{self, CastContext},
    error::Result,
    executor::{
        DatabaseState, StatementExecutionContext,
        expressions::{compare_values, infer_window_return_type},
        normalize_unqualified_object_name, resolve_order_ascending,
        scope::{BoundScope, RowScope},
    },
    txn::{Snapshot, Xid},
    value::{BaseType, PgType, Value},
};

use super::{
    DistinctKey, DistinctPlan, OrderKey, OrderedRow, ProjectionSource, RowOrderSpec,
    are_rows_not_distinct, evaluate_query_expression, evaluate_select_expression,
    evaluate_where_clause, visit_query_source_rows,
};

struct WindowCollector {
    functions: Vec<ast::Function>,
    query_depth: usize,
}

struct WindowMaterializer<'a> {
    functions: &'a [ast::Function],
    values: &'a [Value],
    query_depth: usize,
}

pub(super) fn collect_window_functions(
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
) -> Vec<ast::Function> {
    let mut collector = WindowCollector {
        functions: Vec::new(),
        query_depth: 0,
    };
    let mut visit = |expression: &ast::Expr| {
        let mut expression = expression.clone();
        let _ = expression.visit(&mut collector);
    };
    for projection in projections {
        if let ProjectionSource::Expression(expression) = projection {
            visit(expression);
        }
    }
    for order in order_specs {
        if let OrderKey::Expression(expression) = order.key {
            visit(expression);
        }
    }
    if let DistinctPlan::On { expressions, .. } = distinct {
        for expression in *expressions {
            visit(expression);
        }
    }
    collector.functions
}

fn materialize_window_expression(
    expression: &ast::Expr,
    functions: &[ast::Function],
    values: &[Value],
) -> ast::Expr {
    assert_eq!(functions.len(), values.len());
    let mut expression = expression.clone();
    let _ = expression.visit(&mut WindowMaterializer {
        functions,
        values,
        query_depth: 0,
    });
    expression
}

fn compare_window_order_keys(
    left: &[Value],
    right: &[Value],
    orders: &[ast::OrderByExpr],
) -> Ordering {
    orders
        .iter()
        .zip(left.iter().zip(right))
        .find_map(|(order, (left, right))| {
            let ascending = resolve_order_ascending(&order.options)
                .expect("window ORDER BY variant was validated");
            let nulls_first = order.options.nulls_first.unwrap_or(!ascending);
            let ordering = match (left, right) {
                (Value::Null, Value::Null) => Ordering::Equal,
                (Value::Null, _) => {
                    if nulls_first {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (_, Value::Null) => {
                    if nulls_first {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                _ => {
                    let ordering =
                        compare_values(left, right).expect("window ORDER BY type was validated");
                    if ascending {
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

fn calculate_window_values(
    state: &DatabaseState,
    functions: &[ast::Function],
    scope: &BoundScope,
    rows: &[Vec<Value>],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<Vec<Value>>> {
    let mut values = vec![vec![Value::Null; functions.len()]; rows.len()];
    for (function_index, function) in functions.iter().enumerate() {
        infer_window_return_type(function, RowScope::Bound(scope))?;
        let name = normalize_unqualified_object_name(&function.name)?;
        let ast::WindowType::WindowSpec(window) = function
            .over
            .as_ref()
            .expect("collected window function has OVER")
        else {
            unreachable!("named windows were rejected")
        };
        match name.as_str() {
            "row_number" => {
                let keys = rows
                    .iter()
                    .map(|row| {
                        window
                            .order_by
                            .iter()
                            .map(|order| {
                                evaluate_query_expression(
                                    state,
                                    &order.expr,
                                    scope,
                                    row,
                                    xid,
                                    snapshot,
                                    context,
                                )
                            })
                            .collect::<Result<Vec<_>>>()
                    })
                    .collect::<Result<Vec<_>>>()?;
                let mut indexes = (0..rows.len()).collect::<Vec<_>>();
                indexes.sort_by(|left, right| {
                    compare_window_order_keys(&keys[*left], &keys[*right], &window.order_by)
                });
                for (position, row_index) in indexes.into_iter().enumerate() {
                    values[row_index][function_index] = Value::Int8(
                        i64::try_from(position + 1).expect("row number must fit in int8"),
                    );
                }
            }
            "count" => {
                let keys = rows
                    .iter()
                    .map(|row| {
                        evaluate_query_expression(
                            state,
                            &window.partition_by[0],
                            scope,
                            row,
                            xid,
                            snapshot,
                            context,
                        )
                    })
                    .collect::<Result<Vec<_>>>()?;
                for index in 0..rows.len() {
                    let count = keys.iter().try_fold(0_i64, |count, candidate| {
                        Ok(
                            if are_rows_not_distinct(
                                std::slice::from_ref(&keys[index]),
                                std::slice::from_ref(candidate),
                            )? {
                                count + 1
                            } else {
                                count
                            },
                        )
                    })?;
                    values[index][function_index] = Value::Int8(count);
                }
            }
            _ => unreachable!("window function name was validated"),
        }
    }
    Ok(values)
}

pub(super) fn execute_windowed_select_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    functions: &[ast::Function],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<OrderedRow>> {
    let mut source_rows = Vec::new();
    visit_query_source_rows(
        state,
        select,
        scope,
        xid,
        snapshot,
        context,
        select.selection.as_ref(),
        &mut |row| {
            if evaluate_where_clause(
                state,
                select.selection.as_ref(),
                scope,
                row,
                xid,
                snapshot,
                context,
            )? {
                source_rows.push(row.to_vec());
            }
            Ok(())
        },
    )?;
    let window_values = calculate_window_values(
        state,
        functions,
        scope,
        &source_rows,
        xid,
        snapshot,
        context,
    )?;
    source_rows
        .iter()
        .zip(&window_values)
        .map(|(row, window_values)| {
            let values = projections
                .iter()
                .map(|projection| match projection {
                    ProjectionSource::Column(index) => Ok(row[*index].clone()),
                    ProjectionSource::Merged(slots, data_type, _) => {
                        let value = slots
                            .iter()
                            .map(|slot| &row[*slot])
                            .find(|value| !value.is_null())
                            .cloned()
                            .unwrap_or(Value::Null);
                        if value.is_null() {
                            Ok(value)
                        } else {
                            coercion::coerce(
                                value.clone(),
                                value
                                    .get_base_type()
                                    .expect("non-null value has a base type"),
                                *data_type,
                                CastContext::Implicit,
                            )
                        }
                    }
                    ProjectionSource::Expression(expression) => evaluate_select_expression(
                        state,
                        &materialize_window_expression(expression, functions, window_values),
                        scope,
                        row,
                        None,
                        xid,
                        snapshot,
                        context,
                    ),
                })
                .collect::<Result<Vec<_>>>()?;
            let keys = order_specs
                .iter()
                .map(|order| match order.key {
                    OrderKey::Output(index) => Ok(values[index].clone()),
                    OrderKey::Input(slot, _) => Ok(row[slot].clone()),
                    OrderKey::Expression(expression) => evaluate_select_expression(
                        state,
                        &materialize_window_expression(expression, functions, window_values),
                        scope,
                        row,
                        None,
                        xid,
                        snapshot,
                        context,
                    ),
                })
                .collect::<Result<Vec<_>>>()?;
            let distinct_keys = match distinct {
                DistinctPlan::On { keys: distinct, .. } => distinct
                    .iter()
                    .map(|key| match key {
                        DistinctKey::Output(index) => Ok(values[*index].clone()),
                        DistinctKey::Order(index) => Ok(keys[*index].clone()),
                        DistinctKey::Expression(expression) => evaluate_select_expression(
                            state,
                            &materialize_window_expression(expression, functions, window_values),
                            scope,
                            row,
                            None,
                            xid,
                            snapshot,
                            context,
                        ),
                    })
                    .collect::<Result<Vec<_>>>()?,
                DistinctPlan::None | DistinctPlan::Rows => Vec::new(),
            };
            Ok(OrderedRow {
                values,
                keys,
                distinct_keys,
                deferred_source: None,
                evaluated_projections: None,
            })
        })
        .collect()
}

impl ast::VisitorMut for WindowCollector {
    type Break = ();

    fn pre_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth -= 1;
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if self.query_depth == 0
            && let ast::Expr::Function(function) = expression
            && function.over.is_some()
            && !self.functions.contains(function)
        {
            self.functions.push(function.clone());
        }
        std::ops::ControlFlow::Continue(())
    }
}

impl ast::VisitorMut for WindowMaterializer<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth -= 1;
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if self.query_depth == 0
            && let ast::Expr::Function(function) = expression
            && let Some(index) = self
                .functions
                .iter()
                .position(|candidate| candidate == function)
        {
            *expression = crate::analyzer::create_typed_literal(
                self.values[index].clone(),
                PgType::create(BaseType::Int8),
            );
        }
        std::ops::ControlFlow::Continue(())
    }
}
