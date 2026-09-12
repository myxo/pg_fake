use crate::{
    QueryResult, StatementResult,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{
        DatabaseState, StatementContext,
        ctes::{contains_query_ctes, materialize_query_ctes},
        expressions::infer_window_return_type,
        scope::{RowScope, bind_select_scope},
        views::expand_query_views,
    },
    txn::{RowLockMode, Snapshot, Xid},
    value::Value,
};
use sqlparser::ast;

mod distinct;
mod expressions;
mod grouping;
mod limits;
mod ordering;
mod projection;
mod select;
pub(super) mod set_operations;
mod streaming;
mod values;
mod windows;

use distinct::{DistinctPlan, compare_distinct_keys, remove_duplicate_rows, resolve_distinct_plan};
pub(super) use expressions::infer_query_expression_type;
pub(crate) use grouping::collect_query_primary_key_dependencies;
pub(super) use grouping::contains_query_aggregate;
use grouping::{execute_grouped_select_rows, inspect_aggregate_usage, resolve_grouping_plan};
pub(super) use limits::{has_zero_limit, resolve_select_limit};
use ordering::{RowOrderSpec, compare_ordered_rows, resolve_order_specs, sort_ordered_rows};
pub(crate) use projection::describe_query_result_columns;
pub(super) use projection::{
    ProjectionSource, build_mutation_projection_plan, build_projection_plan,
    evaluate_projection_values,
};
use select::execute_plain_select_rows;
pub(super) use select::validate_select_predicates;
pub(super) use streaming::{QueryStreamState, stream_query_rows};
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
    context: &StatementContext,
) -> Result<StatementResult> {
    if let Some(expanded) = expand_query_views(&state.catalog, query)? {
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
