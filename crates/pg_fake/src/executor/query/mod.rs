use super::*;
use sqlparser::ast;

mod distinct;
mod expressions;
mod grouping;
mod limits;
mod ordering;
mod projection;
pub(super) mod set_operations;
mod values;
mod windows;

use super::ctes::{contains_query_ctes, materialize_query_ctes};
use super::subqueries::evaluate_query_expression;
use super::{
    equality::create_equality_key,
    from::{is_selection_fully_pushed, visit_query_source_rows},
    scope::try_resolve_column_reference,
};
use distinct::{
    DistinctKey, DistinctPlan, compare_distinct_keys, evaluate_distinct_keys,
    remove_duplicate_rows, resolve_distinct_plan,
};
pub(super) use expressions::infer_query_expression_type;
use expressions::{contains_volatile_expression, evaluate_select_expression};
pub(crate) use grouping::collect_query_primary_key_dependencies;
pub(super) use grouping::contains_query_aggregate;
use grouping::{
    AggregateOwner, collect_group_aggregate_functions, collect_grouped_select_rows,
    evaluate_group_having, execute_grouped_select_rows, inspect_aggregate_usage,
    materialize_aggregate_expression, resolve_grouping_plan,
};
pub(super) use limits::{has_zero_limit, resolve_select_limit};
use ordering::{
    OrderKey, RowOrderSpec, compare_order_keys, compare_ordered_rows, evaluate_order_keys,
    resolve_order_specs, retain_top_ordered_row, sort_ordered_rows,
};
pub(crate) use projection::describe_query_result_columns;
pub(super) use projection::{
    ProjectionSource, build_mutation_projection_plan, build_projection_plan,
    evaluate_projection_values,
};
use projection::{
    contains_volatile_projection, create_projection_expression, evaluate_projection_value,
};
use set_operations::{coerce_set_rows, create_set_operand_query};
use values::execute_values_query;
use windows::{collect_window_functions, execute_windowed_select_rows};

struct StatementFeatureDetector {
    cte: bool,
    subquery: bool,
}

impl ast::Visitor for StatementFeatureDetector {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.cte |= query.with.is_some();
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expr: &ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        self.subquery |= matches!(
            expr,
            ast::Expr::Subquery(_) | ast::Expr::Exists { .. } | ast::Expr::InSubquery { .. }
        ) || matches!(
            expr,
            ast::Expr::AnyOp { right, .. } | ast::Expr::AllOp { right, .. }
                if matches!(right.as_ref(), ast::Expr::Subquery(_))
        );
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn detect_statement_features(statement: &ast::Statement) -> (bool, bool) {
    let mut detector = StatementFeatureDetector {
        cte: false,
        subquery: false,
    };
    let _ = ast::Visit::visit(statement, &mut detector);
    (detector.cte, detector.subquery)
}

#[derive(Clone)]
pub(super) struct SelectRow {
    values: Vec<Value>,
    keys: Vec<Value>,
    distinct_keys: Vec<Value>,
    deferred_source: Option<Vec<Value>>,
    evaluated_projections: Option<Vec<bool>>,
}

#[derive(Clone)]
pub(super) struct GroupedStreamRow {
    values: Vec<Value>,
    keys: Vec<Value>,
    source: Vec<Value>,
    evaluated_projections: Vec<bool>,
    deferred_projection_expressions: Vec<Option<ast::Expr>>,
}

#[derive(Clone)]
pub(super) enum PreparedQueryStream {
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
        left_state: Option<Box<PreparedQueryStream>>,
        right_state: Option<Box<PreparedQueryStream>>,
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn resolve_select_lock_mode(query: &ast::Query) -> Result<Option<RowLockMode>> {
    if query.locks.len() > 1 {
        return reject_unsupported("multiple row-lock clauses are not implemented");
    }
    let Some(lock) = query.locks.first() else {
        return Ok(None);
    };
    if lock.of.is_some() || lock.nonblock.is_some() {
        return reject_unsupported("row-lock clause variant is not implemented");
    }
    Ok(Some(match lock.lock_type {
        ast::LockType::Share => RowLockMode::Share,
        ast::LockType::Update => RowLockMode::Update,
    }))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn validate_select_predicates(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
) -> Result<()> {
    if let Some(selection) = &select.selection {
        let base = infer_query_expression_type(state, selection, scope)?.base;
        if base != BaseType::Bool && !is_null_literal(selection) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    if let Some(having) = &select.having {
        let base = infer_query_expression_type(state, having, scope)?.base;
        if base != BaseType::Bool && !is_null_literal(having) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "HAVING requires a boolean expression",
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_where_clause(
    state: &DatabaseState,
    selection: Option<&ast::Expr>,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<bool> {
    let Some(selection) = selection else {
        return Ok(true);
    };
    Ok(
        match evaluate_query_expression(state, selection, scope, row, xid, snapshot, context)? {
            Value::Bool(value) => value,
            Value::Null => false,
            _ => unreachable!("WHERE expression was type-checked"),
        },
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_plain_select_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    top_k: Option<usize>,
) -> Result<Vec<SelectRow>> {
    if let Some(rows) = execute_correlated_exists_rows(
        state,
        select,
        scope,
        projections,
        order_specs,
        distinct,
        xid,
        snapshot,
        context,
    ) {
        return rows;
    }
    if let Some(rows) = execute_any_membership_rows(
        state,
        select,
        scope,
        projections,
        order_specs,
        distinct,
        xid,
        snapshot,
        context,
    ) {
        return rows;
    }
    let mut rows = Vec::new();
    let defer_projection = !order_specs.is_empty() && matches!(distinct, DistinctPlan::None);
    let remaining_selection = if is_selection_fully_pushed(select, scope) {
        None
    } else {
        select.selection.as_ref()
    };
    visit_query_source_rows(
        state,
        select,
        scope,
        xid,
        snapshot,
        context,
        select.selection.as_ref(),
        &mut |row| {
            if order_specs.is_empty() && top_k.is_some_and(|top_k| rows.len() >= top_k) {
                return Ok(());
            }
            if !evaluate_where_clause(
                state,
                remaining_selection,
                scope,
                row,
                xid,
                snapshot,
                context,
            )? {
                return Ok(());
            }
            if defer_projection {
                let mut values = vec![Value::Null; projections.len()];
                let mut evaluated = vec![false; projections.len()];
                for index in order_specs.iter().filter_map(|order| match order.key {
                    OrderKey::Output(index) => Some(index),
                    OrderKey::Input(_, _) | OrderKey::Expression(_) => None,
                }) {
                    if !evaluated[index] {
                        values[index] = evaluate_projection_value(
                            state,
                            &projections[index],
                            scope,
                            row,
                            None,
                            xid,
                            snapshot,
                            context,
                        )?;
                        evaluated[index] = true;
                    }
                }
                let keys = evaluate_order_keys(
                    state,
                    order_specs,
                    &values,
                    scope,
                    row,
                    None,
                    xid,
                    snapshot,
                    context,
                )?;
                retain_top_ordered_row(
                    &mut rows,
                    SelectRow {
                        values,
                        keys,
                        distinct_keys: Vec::new(),
                        deferred_source: Some(row.to_vec()),
                        evaluated_projections: Some(evaluated),
                    },
                    top_k,
                    order_specs,
                );
                return Ok(());
            }
            let values = evaluate_projection_values(
                state,
                projections,
                scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            let keys = evaluate_order_keys(
                state,
                order_specs,
                &values,
                scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            let distinct_keys = evaluate_distinct_keys(
                state, distinct, &values, &keys, scope, row, None, xid, snapshot, context,
            )?;
            retain_top_ordered_row(
                &mut rows,
                SelectRow {
                    values,
                    keys,
                    distinct_keys,
                    deferred_source: None,
                    evaluated_projections: None,
                },
                top_k,
                order_specs,
            );
            Ok(())
        },
    )?;
    if defer_projection {
        sort_ordered_rows(&mut rows, order_specs);
        for row in &mut rows {
            let source = row
                .deferred_source
                .take()
                .expect("deferred projection retains its source row");
            let evaluated = row
                .evaluated_projections
                .take()
                .expect("deferred projection tracks evaluated outputs");
            for (index, projection) in projections.iter().enumerate() {
                if !evaluated[index] {
                    row.values[index] = evaluate_projection_value(
                        state, projection, scope, &source, None, xid, snapshot, context,
                    )?;
                }
            }
        }
    }
    Ok(rows)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_correlated_exists_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Option<Result<Vec<SelectRow>>> {
    let ast::Expr::Exists {
        subquery,
        negated: false,
    } = select.selection.as_ref()?
    else {
        return None;
    };
    let ast::SetExpr::Select(inner_select) = subquery.body.as_ref() else {
        return None;
    };
    let [
        ast::TableWithJoins {
            relation: ast::TableFactor::Table {
                name, args: None, ..
            },
            joins,
        },
    ] = inner_select.from.as_slice()
    else {
        return None;
    };
    let ast::GroupByExpr::Expressions(group_by, modifiers) = &inner_select.group_by else {
        return None;
    };
    let Some(ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::Eq,
        right,
    }) = inner_select.selection.as_ref()
    else {
        return None;
    };
    if !joins.is_empty()
        || inner_select.distinct.is_some()
        || !group_by.is_empty()
        || !modifiers.is_empty()
        || inner_select.having.is_some()
        || inner_select.into.is_some()
        || subquery.with.is_some()
        || subquery.order_by.is_some()
        || subquery.limit_clause.is_some()
        || subquery.fetch.is_some()
    {
        return None;
    }
    let inner_scope = match bind_select_scope(state, inner_select) {
        Ok(scope) => scope,
        Err(error) => return Some(Err(error)),
    };
    let (inner_slot, inner_type, outer_slot, outer_type) = [
        (left.as_ref(), right.as_ref()),
        (right.as_ref(), left.as_ref()),
    ]
    .into_iter()
    .find_map(|(inner, outer)| {
        let (inner_slot, inner_type) = try_resolve_column_reference(inner, &inner_scope)?;
        let (outer_slot, outer_type) = try_resolve_column_reference(outer, scope)?;
        try_resolve_column_reference(outer, &inner_scope)
            .is_none()
            .then_some((inner_slot, inner_type, outer_slot, outer_type))
    })?;
    if inner_type != outer_type {
        return None;
    }
    let table_name = match normalize_relation_name(name) {
        Ok(name) => name,
        Err(error) => return Some(Err(error)),
    };
    let schema = match state.catalog.require_named_table(&table_name) {
        Ok(schema) => schema,
        Err(error) => return Some(Err(error)),
    };
    let mut matches = std::collections::HashSet::new();
    for (_, chain) in state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .iterate_version_chains()
    {
        if let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions)
            && let Some(key) = create_equality_key(&version.row[inner_slot])
        {
            matches.insert(key);
        }
    }
    let mut rows = Vec::new();
    let result = visit_query_source_rows(
        state,
        select,
        scope,
        xid,
        snapshot,
        context,
        None,
        &mut |row| {
            let Some(key) = create_equality_key(&row[outer_slot]) else {
                return Ok(());
            };
            if !matches.contains(&key) {
                return Ok(());
            }
            let values = evaluate_projection_values(
                state,
                projections,
                scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            let keys = evaluate_order_keys(
                state,
                order_specs,
                &values,
                scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            let distinct_keys = evaluate_distinct_keys(
                state, distinct, &values, &keys, scope, row, None, xid, snapshot, context,
            )?;
            rows.push(SelectRow {
                values,
                keys,
                distinct_keys,
                deferred_source: None,
                evaluated_projections: None,
            });
            Ok(())
        },
    );
    Some(result.map(|()| rows))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_any_membership_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Option<Result<Vec<SelectRow>>> {
    let ast::Expr::AnyOp {
        left,
        compare_op: ast::BinaryOperator::Eq,
        right,
        ..
    } = select.selection.as_ref()?
    else {
        return None;
    };
    let ast::Expr::Tuple(candidates) = right.as_ref() else {
        return None;
    };
    if !candidates.iter().all(|candidate| {
        matches!(candidate, ast::Expr::Cast { expr, .. } if matches!(expr.as_ref(), ast::Expr::Value(_)))
    }) {
        return None;
    }
    let left_type = match infer_query_expression_type(state, left, scope) {
        Ok(data_type) => data_type,
        Err(error) => return Some(Err(error)),
    };
    if !matches!(
        left_type.base,
        BaseType::Bool
            | BaseType::Int2
            | BaseType::Int4
            | BaseType::Int8
            | BaseType::Text
            | BaseType::Varchar
            | BaseType::Bpchar
            | BaseType::Bytea
            | BaseType::Uuid
    ) {
        return None;
    }
    let empty_row = vec![Value::Null; scope.columns.len()];
    let mut matches = std::collections::HashSet::new();
    for candidate in candidates {
        let value = match evaluate_and_coerce(
            candidate,
            left_type.base,
            CastContext::Implicit,
            RowScope::Bound(scope),
            &empty_row,
            context,
        ) {
            Ok(value) => value,
            Err(error) => return Some(Err(error)),
        };
        if let Some(key) = create_equality_key(&value) {
            matches.insert(key);
        }
    }
    let mut rows = Vec::new();
    let result = visit_query_source_rows(
        state,
        select,
        scope,
        xid,
        snapshot,
        context,
        None,
        &mut |row| {
            let value = evaluate_and_coerce(
                left,
                left_type.base,
                CastContext::Implicit,
                RowScope::Bound(scope),
                row,
                context,
            )?;
            let Some(key) = create_equality_key(&value) else {
                return Ok(());
            };
            if !matches.contains(&key) {
                return Ok(());
            }
            let values = evaluate_projection_values(
                state,
                projections,
                scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            let keys = evaluate_order_keys(
                state,
                order_specs,
                &values,
                scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            let distinct_keys = evaluate_distinct_keys(
                state, distinct, &values, &keys, scope, row, None, xid, snapshot, context,
            )?;
            rows.push(SelectRow {
                values,
                keys,
                distinct_keys,
                deferred_source: None,
                evaluated_projections: None,
            });
            Ok(())
        },
    );
    Some(result.map(|()| rows))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn finalize_select_rows(
    mut rows: Vec<SelectRow>,
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<Vec<Value>>> {
    if matches!(distinct, DistinctPlan::Rows) {
        rows = remove_duplicate_rows(rows, distinct)?;
    }
    if !order_specs.is_empty()
        && matches!(distinct, DistinctPlan::None)
        && let Some(limit) = limit
    {
        let required = offset.saturating_add(limit);
        if required < rows.len() {
            rows.select_nth_unstable_by(required, |left, right| {
                compare_ordered_rows(left, right, order_specs)
            });
            rows.truncate(required);
        }
    }
    if order_specs.is_empty() && matches!(distinct, DistinctPlan::On { .. }) {
        rows.sort_by(compare_distinct_keys);
    } else {
        sort_ordered_rows(&mut rows, order_specs);
    }
    if matches!(distinct, DistinctPlan::On { .. }) {
        rows = remove_duplicate_rows(rows, distinct)?;
    }
    Ok(rows
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .map(|row| row.values)
        .collect())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_query(
    state: &DatabaseState,
    query: &ast::Query,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    if let Some(expanded) = super::views::expand_query_views(&state.catalog, query)? {
        return execute_query(state, &expanded, xid, snapshot, context);
    }
    if contains_query_ctes(query) {
        let materialized = materialize_query_ctes(state, query, xid, snapshot, context)?;
        if &materialized != query {
            return execute_query(state, &materialized, xid, snapshot, context);
        }
    }
    if query.fetch.is_some() {
        return reject_unsupported("query clause is not implemented");
    }
    let lock_mode = resolve_select_lock_mode(query)?;
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        if let ast::SetExpr::Values(values) = query.body.as_ref() {
            return execute_values_query(query, values, context);
        }
        if lock_mode.is_some() {
            return reject_unsupported("FOR UPDATE is not allowed with set operations");
        }
        return set_operations::execute_set_query(state, query, xid, snapshot, context)
            .map(StatementResult::Query);
    };
    let ast::GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
        return reject_unsupported("GROUP BY is not implemented");
    };
    if select.into.is_some() || !modifiers.is_empty() {
        return reject_unsupported("SELECT feature is not implemented");
    }

    let scope = bind_select_scope(state, select)?;
    if let Some(selection) = &select.selection
        && inspect_aggregate_usage(state, selection, &scope)?.found
    {
        return Err(PgError::create(
            SqlState::GroupingError,
            "aggregate functions are not allowed in WHERE",
        ));
    }
    let (limit, offset) = resolve_select_limit(query, context)?;
    validate_select_predicates(state, select, &scope)?;

    let (projections, columns) = build_projection_plan(state, &select.projection, &scope)?;
    let order_specs = resolve_order_specs(state, query, &projections, &columns, &scope)?;
    let distinct =
        resolve_distinct_plan(state, select, &projections, &columns, &order_specs, &scope)?;
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
    let window_functions = collect_window_functions(&projections, &order_specs, &distinct);
    for function in &window_functions {
        infer_window_return_type(function, RowScope::Bound(&scope))?;
    }
    if !window_functions.is_empty() && grouping.enabled {
        return reject_unsupported("aggregate and window composition is not implemented");
    }
    if grouping.enabled && lock_mode.is_some() {
        return reject_unsupported("FOR UPDATE is not allowed with aggregate functions");
    }
    if !matches!(distinct, DistinctPlan::None) && lock_mode.is_some() {
        return reject_unsupported("FOR UPDATE is not allowed with DISTINCT clause");
    }

    let rows = if !window_functions.is_empty() {
        if lock_mode.is_some() {
            return reject_unsupported("FOR UPDATE is not allowed with window functions");
        }
        execute_windowed_select_rows(
            state,
            select,
            &scope,
            &projections,
            &order_specs,
            &distinct,
            &window_functions,
            xid,
            snapshot,
            context,
        )?
    } else if grouping.enabled {
        execute_grouped_select_rows(
            state,
            select,
            &scope,
            &projections,
            &order_specs,
            &distinct,
            &grouping.expressions,
            xid,
            snapshot,
            context,
        )?
    } else {
        let top_k = if matches!(distinct, DistinctPlan::None) {
            limit.map(|limit| offset.saturating_add(limit))
        } else {
            None
        };
        execute_plain_select_rows(
            state,
            select,
            &scope,
            &projections,
            &order_specs,
            &distinct,
            xid,
            snapshot,
            context,
            top_k,
        )?
    };
    let rows = finalize_select_rows(rows, &order_specs, &distinct, limit, offset)?;
    Ok(StatementResult::Query(QueryResult { columns, rows }))
}

const STREAM_ROW_LIMIT_REACHED: &str = "pg_fake stream row limit reached";

pub(super) fn stream_plain_query_rows(
    state: &DatabaseState,
    query: &ast::Query,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    maximum_rows: Option<usize>,
    prepared: &mut Option<PreparedQueryStream>,
    consume: &mut dyn FnMut(Vec<Value>, &[ColumnMeta]) -> Result<()>,
) -> Result<Option<Vec<ColumnMeta>>> {
    if matches!(prepared, Some(PreparedQueryStream::Materialized { .. })) {
        return Ok(None);
    }
    let cached_query = match prepared.as_ref() {
        Some(PreparedQueryStream::Unordered { query, .. })
        | Some(PreparedQueryStream::Ordered { query, .. })
        | Some(PreparedQueryStream::Grouped { query, .. })
        | Some(PreparedQueryStream::UnionAll { query, .. }) => Some(query.clone()),
        Some(PreparedQueryStream::Materialized { .. }) => unreachable!("handled above"),
        None => None,
    };
    let query = cached_query.as_ref().unwrap_or(query);
    if let Some(expanded) = super::views::expand_query_views(&state.catalog, query)? {
        return stream_plain_query_rows(
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
            return stream_plain_query_rows(
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
        return stream_plain_query_rows(
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
            *prepared = Some(PreparedQueryStream::UnionAll {
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
            let Some(PreparedQueryStream::UnionAll {
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
            let result = stream_plain_query_rows(
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
                    let Some(PreparedQueryStream::Materialized { result, next }) = nested.as_mut()
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
        *prepared = Some(PreparedQueryStream::Materialized { result, next: 0 });
        return Ok(None);
    };
    let ast::GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
        let StatementResult::Query(result) = execute_query(state, query, xid, snapshot, context)?
        else {
            unreachable!("query execution returns rows")
        };
        *prepared = Some(PreparedQueryStream::Materialized { result, next: 0 });
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
        *prepared = Some(PreparedQueryStream::Materialized { result, next: 0 });
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
            *prepared = Some(PreparedQueryStream::Grouped {
                query: query.clone(),
                rows,
                next: 0,
            });
        }
        let Some(PreparedQueryStream::Grouped { rows, next, .. }) = prepared.as_mut() else {
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
            *prepared = Some(PreparedQueryStream::Ordered {
                query: query.clone(),
                rows,
                next: 0,
            });
        }
        let Some(PreparedQueryStream::Ordered { rows, next, .. }) = prepared.as_mut() else {
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
        *prepared = Some(PreparedQueryStream::Unordered {
            query: query.clone(),
            visited: 0,
            eligible: 0,
        });
    }
    let Some(PreparedQueryStream::Unordered {
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
