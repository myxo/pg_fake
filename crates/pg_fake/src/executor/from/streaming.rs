use sqlparser::ast::{self, Spanned as _};

use super::{
    RowConsumer, SourceRow, joins::evaluate_join_condition, materialize_table_factor_rows,
    subqueries::push_derived_filters,
};
use crate::{
    error::Result,
    executor::{
        DatabaseState, StatementContext,
        lateral::bind_lateral_query,
        query,
        scope::{self, BoundScope},
    },
    txn::{Snapshot, Xid},
    value::Value,
};

enum SourcePlan<'a> {
    Empty,
    Factor {
        factor: &'a ast::TableFactor,
        scope: BoundScope,
        start: usize,
    },
    Join {
        left: Box<SourcePlan<'a>>,
        right: Box<SourcePlan<'a>>,
        operator: Option<&'a ast::JoinOperator>,
        scope: BoundScope,
        start: usize,
        middle: usize,
        end: usize,
    },
}

fn build_source_plan<'a>(
    state: &DatabaseState,
    from: &'a [ast::TableWithJoins],
    scope: &BoundScope,
    start: usize,
) -> Result<SourcePlan<'a>> {
    let mut plan = SourcePlan::Empty;
    let mut next = start;
    for table in from {
        let middle = next;
        let right = build_join_plan(state, table, scope, &mut next)?;
        plan = match plan {
            SourcePlan::Empty => right,
            left => SourcePlan::Join {
                left: Box::new(left),
                right: Box::new(right),
                operator: None,
                scope: scope.clone(),
                start,
                middle,
                end: next,
            },
        };
    }
    Ok(plan)
}

fn build_join_plan<'a>(
    state: &DatabaseState,
    table: &'a ast::TableWithJoins,
    scope: &BoundScope,
    next: &mut usize,
) -> Result<SourcePlan<'a>> {
    let start = *next;
    let mut plan = build_factor_plan(state, &table.relation, scope, next)?;
    for join in &table.joins {
        let middle = *next;
        let right = build_factor_plan(state, &join.relation, scope, next)?;
        plan = SourcePlan::Join {
            left: Box::new(plan),
            right: Box::new(right),
            operator: Some(&join.join_operator),
            scope: scope.clone(),
            start,
            middle,
            end: *next,
        };
    }
    Ok(plan)
}

fn build_factor_plan<'a>(
    state: &DatabaseState,
    factor: &'a ast::TableFactor,
    scope: &BoundScope,
    next: &mut usize,
) -> Result<SourcePlan<'a>> {
    let start = *next;
    if let ast::TableFactor::NestedJoin {
        table_with_joins, ..
    } = factor
    {
        let mut nested = BoundScope {
            columns: scope.columns[..start].to_vec(),
        };
        scope::bind_table_with_joins(&state.catalog, table_with_joins, &mut nested)?;
        let end = nested.columns.len();
        nested.columns.extend_from_slice(&scope.columns[end..]);
        return build_join_plan(state, table_with_joins, &nested, next);
    }
    let mut bound = BoundScope {
        columns: scope.columns[..start].to_vec(),
    };
    scope::bind_table_factor(&state.catalog, factor, &mut bound)?;
    *next = bound.columns.len();
    Ok(SourcePlan::Factor {
        factor,
        scope: scope.clone(),
        start,
    })
}

pub(super) fn visit_demand_source_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    selection: Option<&ast::Expr>,
    visit: &mut RowConsumer<'_>,
) -> Result<()> {
    let plan = build_source_plan(state, &select.from, scope, 0)?;
    visit_source_plan(
        state,
        &plan,
        xid,
        snapshot,
        context,
        selection,
        &SourceRow::create(vec![Value::Null; scope.columns.len()]),
        &mut |row| visit(&row.values, &row.origins),
    )
}

fn visit_source_plan(
    state: &DatabaseState,
    plan: &SourcePlan<'_>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    selection: Option<&ast::Expr>,
    prefix: &SourceRow,
    visit: &mut dyn FnMut(SourceRow) -> Result<()>,
) -> Result<()> {
    match plan {
        SourcePlan::Empty => visit(prefix.clone()),
        SourcePlan::Join {
            left,
            right,
            operator,
            scope,
            start,
            middle,
            ..
        } => {
            let preserve_left = operator.is_some_and(|operator| {
                matches!(
                    operator,
                    ast::JoinOperator::Left(_)
                        | ast::JoinOperator::LeftOuter(_)
                        | ast::JoinOperator::FullOuter(_)
                )
            });
            let preserve_right = operator.is_some_and(|operator| {
                matches!(
                    operator,
                    ast::JoinOperator::Right(_)
                        | ast::JoinOperator::RightOuter(_)
                        | ast::JoinOperator::FullOuter(_)
                )
            });
            let reversed = preserve_right && !preserve_left;
            let (outer, inner) = if reversed {
                (right, left)
            } else {
                (left, right)
            };
            let mut matched_inner = std::collections::BTreeSet::new();
            let mut outer_index = 0;
            visit_source_plan(
                state,
                outer,
                xid,
                snapshot,
                context,
                selection,
                prefix,
                &mut |outer_row| {
                    let mut invocation = context.clone();
                    invocation.query_invocation.push(outer_index);
                    outer_index += 1;
                    let mut matched = false;
                    let mut inner_index = 0;
                    visit_source_plan(
                        state,
                        inner,
                        xid,
                        snapshot,
                        &invocation,
                        selection,
                        &outer_row,
                        &mut |row| {
                            let index = inner_index;
                            inner_index += 1;
                            if let Some(operator) = operator
                                && !evaluate_join_condition(
                                    state,
                                    operator,
                                    &row.values,
                                    scope,
                                    *start,
                                    *middle,
                                    xid,
                                    snapshot,
                                    context,
                                )?
                            {
                                return Ok(());
                            }
                            matched = true;
                            matched_inner.insert(index);
                            visit(row)
                        },
                    )?;
                    if !matched && (preserve_left || reversed) {
                        visit(outer_row)?;
                    }
                    Ok(())
                },
            )?;
            if preserve_left && preserve_right {
                let mut index = 0;
                visit_source_plan(
                    state,
                    right,
                    xid,
                    snapshot,
                    context,
                    selection,
                    prefix,
                    &mut |row| {
                        let matched = matched_inner.contains(&index);
                        index += 1;
                        if !matched {
                            visit(row)?;
                        }
                        Ok(())
                    },
                )?;
            }
            Ok(())
        }
        SourcePlan::Factor {
            factor,
            scope,
            start,
            ..
        } => {
            let ast::TableFactor::Derived {
                lateral, subquery, ..
            } = factor
            else {
                let mut slot = *start;
                for row in materialize_table_factor_rows(
                    state, factor, scope, xid, snapshot, context, selection, &mut slot, prefix,
                )? {
                    visit(prefix.combine(&row))?;
                }
                return Ok(());
            };
            let bound;
            let mut correlated = false;
            let query = if *lateral {
                let outer = BoundScope {
                    columns: scope.columns[..*start].to_vec(),
                };
                let slots;
                (bound, slots) =
                    bind_lateral_query(&state.catalog, subquery, &outer, &prefix.values)?;
                correlated = !slots.is_empty();
                &bound
            } else {
                subquery.as_ref()
            };
            let is_cte = context
                .cte_query_barriers
                .lock()
                .expect("locking CTE mutex is poisoned")
                .contains(query);
            let inherited = (!is_cte && query::requires_nested_locking(query))
                .then(|| {
                    context
                        .source_row_locks
                        .iter()
                        .find(|(span, _)| *span == factor.span())
                        .map(|(_, lock)| *lock)
                })
                .flatten();
            let filtered = if is_cte {
                None
            } else {
                push_derived_filters(state, query, scope, *start, selection, inherited.is_some())?
            };
            let query = filtered.as_ref().unwrap_or(query);
            let mut invocation = context.clone();
            invocation.inherited_row_lock = inherited;
            invocation.lateral_invocation |= *lateral;
            if !correlated || is_cte {
                invocation.query_invocation.clear();
            }
            let mut next = 0;
            loop {
                invocation.query_row_demand = Some(next + 1);
                let output = query::execute_query(state, query, xid, snapshot, &invocation)?;
                let Some(values) = output.result.rows.get(next) else {
                    break;
                };
                let mut row = prefix.clone();
                row.values[*start..*start + values.len()].clone_from_slice(values);
                row.origins
                    .extend(output.origins[next].iter().cloned().map(|mut origin| {
                        origin.source = factor.span();
                        origin.start = Some(*start);
                        origin
                    }));
                visit(row)?;
                next += 1;
                if output.complete {
                    break;
                }
            }
            Ok(())
        }
    }
}

pub(in crate::executor) fn recheck_join_conditions(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    row: &mut SourceRow,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<bool> {
    let plan = build_source_plan(state, &select.from, scope, 0)?;
    recheck_source_plan(state, &plan, row, xid, snapshot, context)
}

fn recheck_source_plan(
    state: &DatabaseState,
    plan: &SourcePlan<'_>,
    row: &mut SourceRow,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<bool> {
    let SourcePlan::Join {
        left,
        right,
        operator,
        scope,
        start,
        middle,
        end,
    } = plan
    else {
        return Ok(true);
    };
    let left_matches = recheck_source_plan(state, left, row, xid, snapshot, context)?;
    let right_matches = recheck_source_plan(state, right, row, xid, snapshot, context)?;
    let matches = left_matches
        && right_matches
        && if let Some(operator) = operator {
            evaluate_join_condition(
                state,
                operator,
                &row.values,
                scope,
                *start,
                *middle,
                xid,
                snapshot,
                context,
            )?
        } else {
            true
        };
    if matches {
        return Ok(true);
    }
    let nullable = match operator {
        Some(ast::JoinOperator::Left(_) | ast::JoinOperator::LeftOuter(_)) if left_matches => {
            *middle..*end
        }
        Some(ast::JoinOperator::Right(_) | ast::JoinOperator::RightOuter(_)) if right_matches => {
            *start..*middle
        }
        _ => return Ok(false),
    };
    row.values[nullable.clone()].fill(Value::Null);
    row.origins
        .retain(|origin| origin.start.is_none_or(|start| !nullable.contains(&start)));
    Ok(true)
}
