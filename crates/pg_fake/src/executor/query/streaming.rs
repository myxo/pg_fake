use super::{
    SelectRow,
    distinct::DistinctPlan,
    execute_query,
    expressions::contains_volatile_expression,
    grouping::{
        AggregateOwner, collect_group_aggregate_functions, collect_grouped_select_rows,
        contains_query_aggregate, evaluate_group_having, materialize_aggregate_expression,
        resolve_grouping_plan,
    },
    limits::resolve_select_limit,
    ordering::{
        OrderKey, compare_order_keys, evaluate_order_keys, resolve_order_specs, sort_ordered_rows,
    },
    projection::{
        ProjectionSource, build_projection_plan, contains_volatile_projection,
        describe_query_result_columns, evaluate_projection_value, evaluate_projection_values,
    },
    select::{evaluate_where_clause, validate_select_predicates},
    set_operations::{coerce_set_rows, create_set_operand_query},
};
use crate::{
    ColumnMeta, QueryResult, StatementResult,
    error::{PgError, Result, SqlState},
    executor::{
        DatabaseState, StatementContext,
        ctes::{contains_query_ctes, materialize_query_ctes},
        from::{is_selection_fully_pushed, visit_query_source_rows},
        scope::bind_select_scope,
        subqueries::evaluate_query_expression,
        views::expand_query_views,
    },
    txn::{Snapshot, Xid},
    value::Value,
};
use sqlparser::ast;

const STREAM_ROW_LIMIT_REACHED: &str = "pg_fake stream row limit reached";

#[derive(Clone)]
pub(in crate::executor) struct GroupedStreamRow {
    values: Vec<Value>,
    keys: Vec<Value>,
    source: Vec<Value>,
    evaluated_projections: Vec<bool>,
    deferred_projection_expressions: Vec<Option<ast::Expr>>,
}

#[derive(Clone)]
pub(in crate::executor) enum QueryStreamState {
    Unordered {
        query: ast::Query,
        visited: usize,
        eligible: usize,
    },
    Ordered {
        query: ast::Query,
        rows: Vec<SelectRow>,
        next: usize,
    },
    Grouped {
        query: ast::Query,
        rows: Vec<GroupedStreamRow>,
        next: usize,
    },
    UnionAll {
        query: ast::Query,
        left: ast::Query,
        right: ast::Query,
        left_state: Option<Box<QueryStreamState>>,
        right_state: Option<Box<QueryStreamState>>,
        reads_right: bool,
        produced: usize,
        limit: Option<usize>,
        offset: usize,
        columns: Vec<ColumnMeta>,
    },
    Materialized {
        result: QueryResult,
        next: usize,
    },
}

pub(in crate::executor) fn stream_query_rows(
    state: &DatabaseState,
    query: &ast::Query,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    maximum_rows: Option<usize>,
    prepared: &mut Option<QueryStreamState>,
    consume: &mut dyn FnMut(Vec<Value>, &[ColumnMeta]) -> Result<()>,
) -> Result<Option<Vec<ColumnMeta>>> {
    if matches!(prepared, Some(QueryStreamState::Materialized { .. })) {
        return Ok(None);
    }
    let cached_query = match prepared.as_ref() {
        Some(QueryStreamState::Unordered { query, .. })
        | Some(QueryStreamState::Ordered { query, .. })
        | Some(QueryStreamState::Grouped { query, .. })
        | Some(QueryStreamState::UnionAll { query, .. }) => Some(query.clone()),
        Some(QueryStreamState::Materialized { .. }) => unreachable!("handled above"),
        None => None,
    };
    let query = cached_query.as_ref().unwrap_or(query);
    if let Some(expanded) = expand_query_views(&state.catalog, query)? {
        return stream_query_rows(
            state,
            &expanded,
            xid,
            snapshot,
            context,
            maximum_rows,
            prepared,
            consume,
        );
    }
    if contains_query_ctes(query) {
        let materialized = materialize_query_ctes(state, query, xid, snapshot, context)?;
        if &materialized != query {
            return stream_query_rows(
                state,
                &materialized,
                xid,
                snapshot,
                context,
                maximum_rows,
                prepared,
                consume,
            );
        }
    }
    if let ast::SetExpr::Query(nested) = query.body.as_ref()
        && query.with.is_none()
        && query.order_by.is_none()
        && query.limit_clause.is_none()
        && query.fetch.is_none()
        && query.locks.is_empty()
        && query.for_clause.is_none()
    {
        return stream_query_rows(
            state,
            nested,
            xid,
            snapshot,
            context,
            maximum_rows,
            prepared,
            consume,
        );
    }
    if let ast::SetExpr::SetOperation {
        op: ast::SetOperator::Union,
        set_quantifier: ast::SetQuantifier::All,
        left,
        right,
    } = query.body.as_ref()
        && query.order_by.is_none()
        && query.fetch.is_none()
        && query.locks.is_empty()
    {
        if prepared.is_none() {
            let (limit, offset) = resolve_select_limit(query, context)?;
            let columns = describe_query_result_columns(
                state,
                &ast::Statement::Query(Box::new(query.clone())),
            )?;
            *prepared = Some(QueryStreamState::UnionAll {
                query: query.clone(),
                left: create_set_operand_query(query, left),
                right: create_set_operand_query(query, right),
                left_state: None,
                right_state: None,
                reads_right: false,
                produced: 0,
                limit,
                offset,
                columns,
            });
        }
        let mut emitted = 0;
        loop {
            let Some(QueryStreamState::UnionAll {
                left,
                right,
                left_state,
                right_state,
                reads_right,
                produced,
                limit,
                offset,
                columns,
                ..
            }) = prepared.as_mut()
            else {
                unreachable!("UNION ALL query has union stream state")
            };
            if limit.is_some_and(|limit| *produced >= offset.saturating_add(limit))
                || maximum_rows.is_some_and(|maximum| emitted >= maximum)
            {
                return Ok(Some(columns.clone()));
            }
            let operand = if *reads_right { right } else { left };
            let operand_state = if *reads_right {
                right_state
            } else {
                left_state
            };
            let mut nested = operand_state.take().map(|state| *state);
            let own_remaining =
                limit.map(|limit| offset.saturating_add(limit).saturating_sub(*produced));
            let caller_remaining = maximum_rows.map(|maximum| {
                offset
                    .saturating_sub(*produced)
                    .saturating_add(maximum.saturating_sub(emitted))
            });
            let nested_maximum = match (own_remaining, caller_remaining) {
                (Some(left), Some(right)) => Some(left.min(right)),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            };
            let result = stream_query_rows(
                state,
                operand,
                xid,
                snapshot,
                context,
                nested_maximum,
                &mut nested,
                &mut |row, source_columns| {
                    let selected = *produced >= *offset
                        && !limit.is_some_and(|limit| *produced >= offset.saturating_add(limit));
                    *produced += 1;
                    if !selected {
                        return Ok(());
                    }
                    emitted += 1;
                    let mut rows = coerce_set_rows(vec![row], source_columns, columns)?;
                    consume(rows.pop().expect("set operand produces one row"), columns)
                },
            );
            if matches!(result, Ok(None)) {
                let drain = (|| {
                    let Some(QueryStreamState::Materialized { result, next }) = nested.as_mut()
                    else {
                        unreachable!("non-streamable UNION ALL operand is materialized")
                    };
                    while *next < result.rows.len() {
                        if limit.is_some_and(|limit| *produced >= offset.saturating_add(limit))
                            || maximum_rows.is_some_and(|maximum| emitted >= maximum)
                        {
                            break;
                        }
                        let row = result.rows[*next].clone();
                        *next += 1;
                        let selected = *produced >= *offset
                            && !limit
                                .is_some_and(|limit| *produced >= offset.saturating_add(limit));
                        *produced += 1;
                        if !selected {
                            continue;
                        }
                        emitted += 1;
                        let mut rows = coerce_set_rows(vec![row], &result.columns, columns)?;
                        consume(rows.pop().expect("set operand produces one row"), columns)?;
                    }
                    Ok(())
                })();
                *operand_state = nested.map(Box::new);
                drain?;
                if limit.is_some_and(|limit| *produced >= offset.saturating_add(limit))
                    || maximum_rows.is_some_and(|maximum| emitted >= maximum)
                {
                    return Ok(Some(columns.clone()));
                }
                if *reads_right {
                    return Ok(Some(columns.clone()));
                }
                *reads_right = true;
                continue;
            }
            *operand_state = nested.map(Box::new);
            let result = match result {
                Err(error) => return Err(error),
                Ok(result) => result,
            };
            if limit.is_some_and(|limit| *produced >= offset.saturating_add(limit))
                || maximum_rows.is_some_and(|maximum| emitted >= maximum)
            {
                return Ok(Some(columns.clone()));
            }
            match result {
                Some(_) if *reads_right => return Ok(Some(columns.clone())),
                Some(_) => *reads_right = true,
                None => unreachable!("materialized UNION ALL operand was drained"),
            }
        }
    }
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        let StatementResult::Query(result) = execute_query(state, query, xid, snapshot, context)?
        else {
            unreachable!("query execution returns rows")
        };
        *prepared = Some(QueryStreamState::Materialized { result, next: 0 });
        return Ok(None);
    };
    let ast::GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
        let StatementResult::Query(result) = execute_query(state, query, xid, snapshot, context)?
        else {
            unreachable!("query execution returns rows")
        };
        *prepared = Some(QueryStreamState::Materialized { result, next: 0 });
        return Ok(None);
    };
    if query.fetch.is_some()
        || !query.locks.is_empty()
        || select.distinct.is_some()
        || !modifiers.is_empty()
    {
        let StatementResult::Query(result) = execute_query(state, query, xid, snapshot, context)?
        else {
            unreachable!("query execution returns rows")
        };
        *prepared = Some(QueryStreamState::Materialized { result, next: 0 });
        return Ok(None);
    }
    let scope = bind_select_scope(state, select)?;
    let (limit, offset) = resolve_select_limit(query, context)?;
    validate_select_predicates(state, select, &scope)?;
    let (projections, columns) = build_projection_plan(state, &select.projection, &scope)?;
    let order_specs = resolve_order_specs(state, query, &projections, &columns, &scope)?;
    if limit == Some(0) || maximum_rows == Some(0) {
        return Ok(Some(columns));
    }
    let grouped =
        !group_by.is_empty() || select.having.is_some() || contains_query_aggregate(query);
    if grouped {
        let distinct = DistinctPlan::None;
        let grouping = resolve_grouping_plan(
            state,
            select,
            group_by,
            &projections,
            &columns,
            &order_specs,
            &distinct,
            &scope,
        )?;
        assert!(grouping.enabled);
        if prepared.is_none() {
            let aggregate_functions = collect_group_aggregate_functions(
                state,
                select,
                &projections,
                &order_specs,
                &distinct,
                &scope,
            )?;
            let groups = collect_grouped_select_rows(
                state,
                select,
                &scope,
                &grouping.expressions,
                &aggregate_functions,
                xid,
                snapshot,
                context,
            )?;
            let mut rows = Vec::new();
            for (source, aggregate_values) in groups {
                if !evaluate_group_having(
                    state,
                    select,
                    &scope,
                    &source,
                    &aggregate_values,
                    xid,
                    snapshot,
                    context,
                )? {
                    continue;
                }
                let mut values = vec![Value::Null; projections.len()];
                let mut evaluated_projections = vec![false; projections.len()];
                let mut deferred_projection_expressions = vec![None; projections.len()];
                for (index, projection) in projections.iter().enumerate() {
                    let required_by_order = order_specs.iter().any(
                        |order| matches!(order.key, OrderKey::Output(output) if output == index),
                    );
                    match projection {
                        ProjectionSource::Expression(expression) => {
                            let expression = materialize_aggregate_expression(
                                state,
                                expression,
                                &scope,
                                &aggregate_values,
                                AggregateOwner::Projection(index),
                            )?;
                            if required_by_order
                                || (!order_specs.is_empty()
                                    && !contains_volatile_expression(&expression))
                            {
                                values[index] = evaluate_query_expression(
                                    state,
                                    &expression,
                                    &scope,
                                    &source,
                                    xid,
                                    snapshot,
                                    context,
                                )?;
                                evaluated_projections[index] = true;
                            } else {
                                deferred_projection_expressions[index] = Some(expression);
                            }
                        }
                        ProjectionSource::Column(_) | ProjectionSource::Merged(_, _, _) => {
                            values[index] = evaluate_projection_value(
                                state,
                                projection,
                                &scope,
                                &source,
                                Some((&aggregate_values, AggregateOwner::Projection(index))),
                                xid,
                                snapshot,
                                context,
                            )?;
                            evaluated_projections[index] = true;
                        }
                    }
                }
                let keys = evaluate_order_keys(
                    state,
                    &order_specs,
                    &values,
                    &scope,
                    &source,
                    Some(&aggregate_values),
                    xid,
                    snapshot,
                    context,
                )?;
                rows.push(GroupedStreamRow {
                    values,
                    keys,
                    source,
                    evaluated_projections,
                    deferred_projection_expressions,
                });
            }
            if !order_specs.is_empty() {
                rows.sort_by(|left, right| {
                    compare_order_keys(&left.keys, &right.keys, &order_specs)
                });
            }
            rows.truncate(
                limit
                    .map(|limit| offset.saturating_add(limit))
                    .unwrap_or(usize::MAX),
            );
            *prepared = Some(QueryStreamState::Grouped {
                query: query.clone(),
                rows,
                next: 0,
            });
        }
        let Some(QueryStreamState::Grouped { rows, next, .. }) = prepared.as_mut() else {
            unreachable!("grouped query has grouped stream state")
        };
        let mut emitted = 0;
        while *next < rows.len() && !maximum_rows.is_some_and(|maximum| emitted >= maximum) {
            let index = *next;
            let row = &mut rows[index];
            for projection_index in 0..projections.len() {
                if !row.evaluated_projections[projection_index] {
                    let expression = row.deferred_projection_expressions[projection_index]
                        .as_ref()
                        .expect("deferred grouped projection retains its expression");
                    row.values[projection_index] = evaluate_query_expression(
                        state,
                        expression,
                        &scope,
                        &row.source,
                        xid,
                        snapshot,
                        context,
                    )?;
                    row.evaluated_projections[projection_index] = true;
                }
            }
            *next += 1;
            if index >= offset {
                emitted += 1;
                consume(row.values.clone(), &columns)?;
            }
        }
        return Ok(Some(columns));
    }
    let remaining_selection = if is_selection_fully_pushed(select, &scope) {
        None
    } else {
        select.selection.as_ref()
    };
    if !order_specs.is_empty() {
        if prepared.is_none() {
            let mut rows = Vec::new();
            visit_query_source_rows(
                state,
                select,
                &scope,
                xid,
                snapshot,
                context,
                select.selection.as_ref(),
                &mut |row| {
                    if !evaluate_where_clause(
                        state,
                        remaining_selection,
                        &scope,
                        row,
                        xid,
                        snapshot,
                        context,
                    )? {
                        return Ok(());
                    }
                    let mut values = vec![Value::Null; projections.len()];
                    let mut evaluated = vec![false; projections.len()];
                    for (index, projection) in projections.iter().enumerate() {
                        let required_by_order = order_specs.iter().any(|order| {
                            matches!(order.key, OrderKey::Output(output) if output == index)
                        });
                        if required_by_order || !contains_volatile_projection(projection) {
                            values[index] = evaluate_projection_value(
                                state, projection, &scope, row, None, xid, snapshot, context,
                            )?;
                            evaluated[index] = true;
                        }
                    }
                    let keys = evaluate_order_keys(
                        state,
                        &order_specs,
                        &values,
                        &scope,
                        row,
                        None,
                        xid,
                        snapshot,
                        context,
                    )?;
                    rows.push(SelectRow {
                        values,
                        keys,
                        distinct_keys: Vec::new(),
                        deferred_source: Some(row.to_vec()),
                        evaluated_projections: Some(evaluated),
                    });
                    Ok(())
                },
            )?;
            sort_ordered_rows(&mut rows, &order_specs);
            rows.truncate(
                limit
                    .map(|limit| offset.saturating_add(limit))
                    .unwrap_or(usize::MAX),
            );
            *prepared = Some(QueryStreamState::Ordered {
                query: query.clone(),
                rows,
                next: 0,
            });
        }
        let Some(QueryStreamState::Ordered { rows, next, .. }) = prepared.as_mut() else {
            unreachable!("ordered query has ordered stream state")
        };
        let mut emitted = 0;
        while *next < rows.len() && !maximum_rows.is_some_and(|maximum| emitted >= maximum) {
            let index = *next;
            let row = &mut rows[index];
            let source = row
                .deferred_source
                .as_ref()
                .expect("deferred projection retains its source row");
            let evaluated = row
                .evaluated_projections
                .as_mut()
                .expect("deferred projection tracks evaluated outputs");
            for (index, projection) in projections.iter().enumerate() {
                if !evaluated[index] {
                    row.values[index] = evaluate_projection_value(
                        state, projection, &scope, &source, None, xid, snapshot, context,
                    )?;
                    evaluated[index] = true;
                }
            }
            *next += 1;
            if index >= offset {
                emitted += 1;
                consume(row.values.clone(), &columns)?;
            }
        }
        return Ok(Some(columns));
    }
    if prepared.is_none() {
        *prepared = Some(QueryStreamState::Unordered {
            query: query.clone(),
            visited: 0,
            eligible: 0,
        });
    }
    let Some(QueryStreamState::Unordered {
        visited, eligible, ..
    }) = prepared.as_mut()
    else {
        unreachable!("unordered query has unordered stream state")
    };
    let already_visited = *visited;
    let mut seen = 0;
    let mut next_visited = already_visited;
    let mut emitted = 0;
    let result = visit_query_source_rows(
        state,
        select,
        &scope,
        xid,
        snapshot,
        context,
        select.selection.as_ref(),
        &mut |row| {
            if seen < already_visited {
                seen += 1;
                return Ok(());
            }
            if maximum_rows.is_some_and(|maximum| emitted >= maximum) {
                return Err(PgError::create(
                    SqlState::InternalError,
                    STREAM_ROW_LIMIT_REACHED,
                ));
            }
            seen += 1;
            next_visited += 1;
            if limit.is_some_and(|limit| *eligible >= offset.saturating_add(limit)) {
                return Ok(());
            }
            if !evaluate_where_clause(
                state,
                remaining_selection,
                &scope,
                row,
                xid,
                snapshot,
                context,
            )? {
                return Ok(());
            }
            let selected = *eligible >= offset;
            *eligible += 1;
            let values = evaluate_projection_values(
                state,
                &projections,
                &scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            if selected {
                emitted += 1;
                consume(values, &columns)?;
            }
            Ok(())
        },
    );
    *visited = next_visited;
    if let Err(error) = result {
        if error.sqlstate != SqlState::InternalError || error.message != STREAM_ROW_LIMIT_REACHED {
            return Err(error);
        }
    }
    Ok(Some(columns))
}
