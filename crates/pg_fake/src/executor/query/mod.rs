use crate::{
    QueryResult,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{
        DatabaseState, StatementContext,
        ctes::{contains_query_ctes, materialize_query_ctes},
        expressions::infer_window_return_type,
        scope::{RowScope, bind_select_scope},
        views::expand_query_views,
    },
    txn::{Snapshot, Xid},
    value::Value,
};
use sqlparser::ast::{self, Spanned as _};

mod distinct;
mod expressions;
mod grouping;
mod limits;
mod locking;
mod ordering;
mod projection;
mod select;
pub(super) mod set_operations;
mod streaming;
mod values;
mod windows;

use distinct::{DistinctPlan, compare_distinct_keys, remove_duplicate_rows, resolve_distinct_plan};
pub(super) use expressions::contains_volatile_expression;
pub(super) use expressions::infer_query_expression_type;
pub(super) use grouping::contains_query_aggregate;
pub(crate) use grouping::{
    PreparedGroupOutput, PreparedGrouping, collect_query_primary_key_dependencies,
};
use grouping::{execute_grouped_select_rows, inspect_aggregate_usage, resolve_grouping_plan};
pub(crate) use limits::PreparedLimit;
pub(super) use limits::{has_zero_limit, resolve_select_limit};
pub(crate) use locking::SelectLock;
pub(super) use locking::{
    contains_locking_operations, requires_nested_locking, resolve_query_lock_targets,
    resolve_select_lock_mode,
};
use ordering::{RowOrderSpec, compare_ordered_rows, resolve_order_specs, sort_ordered_rows};
pub(crate) use projection::describe_query_result_columns;
pub(super) use projection::{
    ProjectionSource, build_mutation_projection_plan, build_projection_plan,
    evaluate_projection_values,
};
pub(crate) use select::PreparedPlainRows;
use select::execute_plain_select_rows;
pub(super) use select::validate_select_predicates;
pub(super) use streaming::{QueryStreamState, stream_query_rows};
pub(crate) use values::PreparedValues;
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

pub(crate) const LOCK_PENDING: &str = "pg_fake pending lock";

#[derive(Clone)]
pub(crate) struct PreparedSelectRows {
    occurrence: sqlparser::tokenizer::Span,
    sql: String,
    rows: Vec<SelectRow>,
    next: usize,
    selected: Vec<SelectRow>,
    source_complete: bool,
}

#[derive(Clone)]
pub(crate) struct PreparedLockQuery {
    pub(crate) occurrence: sqlparser::tokenizer::Span,
    pub(crate) sql: String,
    pub(crate) output: QueryOutput,
    pub(crate) locks: Vec<super::locks::RequiredRowLock>,
}

#[derive(Clone)]
pub(crate) struct QueryOutput {
    pub(crate) result: QueryResult,
    pub(crate) origins: Vec<Vec<super::from::RowOrigin>>,
    pub(crate) complete: bool,
}

impl QueryOutput {
    pub(super) fn create(result: QueryResult) -> Self {
        let origins = vec![Vec::new(); result.rows.len()];
        Self {
            result,
            origins,
            complete: true,
        }
    }
}

#[derive(Clone)]
pub(crate) struct DerivedProjection {
    select: ast::Select,
    source: super::from::SourceRow,
}

#[derive(Clone)]
pub(super) struct SelectRow {
    values: Vec<Value>,
    origins: Vec<super::from::RowOrigin>,
    keys: Vec<Value>,
    distinct_keys: Vec<Value>,
    deferred_source: Option<Vec<Value>>,
    evaluated_projections: Option<Vec<bool>>,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn finalize_select_rows(
    mut rows: Vec<SelectRow>,
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<SelectRow>> {
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
        .collect())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_query(
    state: &DatabaseState,
    query: &ast::Query,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<QueryOutput> {
    let maximum_rows = context.query_row_demand;
    let inherited = context.inherited_row_lock;
    let mut context = context.clone();
    context.advisory.enabled |= crate::advisory::contains_advisory_function(query);
    context.query_row_demand = None;
    context.inherited_row_lock = None;
    context.source_row_locks.clear();
    context.capture_lock_queries |= inherited.is_some()
        || contains_locking_operations(query)
        || context
            .prepared_lock_queries
            .lock()
            .expect("prepared locks mutex is poisoned")
            .iter()
            .any(|cached| cached.occurrence == query.span() && cached.sql == query.to_string());
    context.retain_row_origins |= context.capture_lock_queries;
    if context.capture_lock_queries {
        context
            .query_source_state
            .lock()
            .expect("query source mutex is poisoned")
            .get_or_insert_with(|| std::sync::Arc::new(state.clone()));
    }
    let query_sql = format!("{:?} {inherited:?} {query}", context.query_invocation);
    let context = &context;
    let cache_key = context
        .capture_lock_queries
        .then(|| (query.span(), query_sql.clone()));
    if let Some((occurrence, sql)) = &cache_key {
        let cached = context
            .prepared_lock_queries
            .lock()
            .expect("prepared locks mutex is poisoned")
            .iter()
            .find(|cached| {
                cached.occurrence == *occurrence
                    && &cached.sql == sql
                    && (cached.output.complete
                        || maximum_rows
                            .is_some_and(|maximum| cached.output.result.rows.len() >= maximum))
            })
            .cloned();
        if let Some(mut cached) = cached {
            if let Some(maximum) = maximum_rows {
                if cached.output.result.rows.len() > maximum {
                    cached.output.complete = false;
                }
                cached.output.result.rows.truncate(maximum);
                cached.output.origins.truncate(maximum);
            }
            context
                .select_row_locks
                .lock()
                .expect("select locks mutex is poisoned")
                .extend(cached.locks);
            return Ok(cached.output);
        }
    }
    let lock_start = context
        .select_row_locks
        .lock()
        .expect("select locks mutex is poisoned")
        .len();
    let lock_targets = resolve_query_lock_targets(
        &state.catalog,
        query,
        inherited,
        &[],
        &context
            .cte_query_barriers
            .lock()
            .expect("locking CTE mutex is poisoned"),
    )?;
    if let Some(expanded) = expand_query_views(&state.catalog, query)? {
        let mut expanded_context = context.clone();
        expanded_context.query_row_demand = maximum_rows;
        expanded_context.inherited_row_lock = inherited;
        return execute_query(state, &expanded, xid, snapshot, &expanded_context);
    }
    if contains_query_ctes(query) {
        let materialized = materialize_query_ctes(state, query, xid, snapshot, context)?;
        if &materialized != query {
            let mut materialized_context = context.clone();
            materialized_context.query_row_demand = maximum_rows;
            materialized_context.inherited_row_lock = inherited;
            return execute_query(state, &materialized, xid, snapshot, &materialized_context);
        }
    }
    if let ast::SetExpr::Query(nested) = query.body.as_ref()
        && query.order_by.is_none()
        && query.limit_clause.is_none()
        && query.fetch.is_none()
    {
        let mut nested = nested.as_ref().clone();
        nested.locks.extend(query.locks.iter().cloned());
        let mut nested_context = context.clone();
        nested_context.query_row_demand = maximum_rows;
        nested_context.inherited_row_lock = inherited;
        return execute_query(state, &nested, xid, snapshot, &nested_context);
    }
    if query.fetch.is_some() {
        return reject_unsupported("query clause is not implemented");
    }
    let lock_mode = resolve_select_lock_mode(query);
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        if let ast::SetExpr::Values(values) = query.body.as_ref() {
            return execute_values_query(query, values, context, maximum_rows);
        }
        if lock_mode.is_some() {
            return reject_unsupported("FOR UPDATE is not allowed with set operations");
        }
        return set_operations::execute_set_query(
            state,
            query,
            xid,
            snapshot,
            context,
            maximum_rows,
        );
    };
    let ast::GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
        return reject_unsupported("GROUP BY is not implemented");
    };
    if select.into.is_some() || !modifiers.is_empty() {
        return reject_unsupported("SELECT feature is not implemented");
    }

    let mut context = context.clone();
    context.source_row_locks = lock_targets;
    let mut derived = Vec::new();
    struct DerivedSources<'a>(&'a mut Vec<sqlparser::tokenizer::Span>);
    impl ast::Visitor for DerivedSources<'_> {
        type Break = ();
        fn pre_visit_table_factor(
            &mut self,
            factor: &ast::TableFactor,
        ) -> std::ops::ControlFlow<()> {
            if let ast::TableFactor::Derived { subquery, .. } = factor
                && requires_nested_locking(subquery)
            {
                self.0.push(factor.span());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let _ = ast::Visit::visit(&select.from, &mut DerivedSources(&mut derived));
    let lock_targets = context
        .source_row_locks
        .iter()
        .copied()
        .filter(|(source, _)| !derived.contains(source))
        .collect::<Vec<_>>();
    let context = &context;
    let scope = bind_select_scope(state, select)?;
    if let Some(selection) = &select.selection
        && inspect_aggregate_usage(state, selection, &scope)?.found
    {
        return Err(PgError::create(
            SqlState::GroupingError,
            "aggregate functions are not allowed in WHERE",
        ));
    }
    let (query_limit, offset) = resolve_select_limit(query, context)?;
    let limit = match (query_limit, maximum_rows) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
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

    let pull_candidates = !lock_targets.is_empty() && order_specs.is_empty();
    let mut prepared_rows = {
        let mut prepared = context
            .prepared_select_rows
            .lock()
            .expect("prepared SELECT rows mutex is poisoned");
        prepared
            .iter()
            .position(|cached| cached.occurrence == query.span() && cached.sql == query_sql)
            .map(|index| prepared.remove(index))
    };
    let rows = if let Some(prepared) = &mut prepared_rows {
        std::mem::take(&mut prepared.rows)
    } else if limit == Some(0) {
        Vec::new()
    } else if !window_functions.is_empty() {
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
        let top_k = if pull_candidates {
            Some(1)
        } else if matches!(distinct, DistinctPlan::None) && lock_targets.is_empty() {
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
            !lock_targets.is_empty(),
        )?
    };
    let mut rows = rows;
    let mut complete = maximum_rows.is_none()
        || query_limit.is_some_and(|limit| maximum_rows.is_some_and(|maximum| limit <= maximum));
    if !lock_targets.is_empty() {
        if prepared_rows.is_none() {
            sort_ordered_rows(&mut rows, &order_specs);
        }
        let mut prepared = prepared_rows.unwrap_or_else(|| PreparedSelectRows {
            occurrence: query.span(),
            sql: query_sql.clone(),
            rows: Vec::new(),
            next: 0,
            selected: Vec::new(),
            source_complete: !pull_candidates || rows.is_empty(),
        });
        prepared.rows = rows;
        loop {
            if limit == Some(0)
                || limit
                    .is_some_and(|limit| prepared.selected.len() >= offset.saturating_add(limit))
            {
                break;
            }
            if prepared.next >= prepared.rows.len() {
                if prepared.source_complete {
                    break;
                }
                let requested = prepared.rows.len() + 1;
                context
                    .prepared_select_rows
                    .lock()
                    .expect("prepared SELECT rows mutex is poisoned")
                    .push(prepared.clone());
                let rows = execute_plain_select_rows(
                    state,
                    select,
                    &scope,
                    &projections,
                    &order_specs,
                    &distinct,
                    xid,
                    snapshot,
                    context,
                    Some(requested),
                    true,
                )?;
                context
                    .prepared_select_rows
                    .lock()
                    .expect("prepared SELECT rows mutex is poisoned")
                    .retain(|cached| cached.occurrence != query.span() || cached.sql != query_sql);
                prepared.source_complete = rows.len() < requested;
                prepared.rows = rows;
                if prepared.next >= prepared.rows.len() {
                    break;
                }
            }
            let row = &mut prepared.rows[prepared.next];
            let source = row
                .deferred_source
                .as_mut()
                .expect("locking projection retains source");
            let evaluated = row
                .evaluated_projections
                .as_mut()
                .expect("locking projection retains evaluation state");
            let mut refreshed = super::from::SourceRow {
                values: source.clone(),
                origins: row.origins.clone(),
            };
            let refresh = locking::refresh_locked_source(
                Some(&lock_targets),
                state,
                &mut refreshed,
                xid,
                snapshot,
                context,
            )?;
            let deleted = refresh.is_none();
            let changed = refresh == Some(true);
            let joins_match = !changed
                || super::from::recheck_join_conditions(
                    state,
                    select,
                    &scope,
                    &mut refreshed,
                    xid,
                    snapshot,
                    context,
                )?;
            if changed {
                *source = refreshed.values;
                row.origins = refreshed.origins;
            }
            if deleted
                || !joins_match
                || (changed
                    && !select::evaluate_where_clause(
                        state,
                        select.selection.as_ref(),
                        &scope,
                        source,
                        xid,
                        &context.source_snapshot,
                        context,
                    )?)
            {
                prepared.next += 1;
                continue;
            }
            if changed {
                evaluated.fill(false);
            }
            for (index, projection) in projections.iter().enumerate() {
                if !evaluated[index] {
                    row.values[index] = projection::evaluate_projection_value(
                        state,
                        projection,
                        &scope,
                        source,
                        None,
                        xid,
                        &context.source_snapshot,
                        context,
                    )?;
                    evaluated[index] = true;
                }
            }
            let mut skip = false;
            let mut pending = Vec::new();
            for origin in &row.origins {
                let Some((_, lock)) = lock_targets
                    .iter()
                    .find(|(source, _)| *source == origin.source)
                else {
                    continue;
                };
                if state.row_locks.is_held(origin.key, xid, lock.mode) {
                    continue;
                }
                if state.row_locks.would_block(origin.key, xid, lock.mode) {
                    if !pending.is_empty() {
                        break;
                    }
                    match lock.policy {
                        locking::LockPolicy::SkipLocked => {
                            skip = true;
                            break;
                        }
                        locking::LockPolicy::NoWait => {
                            return Err(PgError::create(
                                SqlState::LockNotAvailable,
                                "could not obtain lock on row",
                            ));
                        }
                        locking::LockPolicy::Block => {}
                    }
                }
                let table = state
                    .tables
                    .get(&origin.key.table_id)
                    .expect("row origin table exists");
                if let Some((_, chain)) = table
                    .iterate_version_chains()
                    .find(|(row_id, _)| *row_id == origin.key.row_id)
                    && let Some(version) =
                        crate::txn::find_visible_version(chain, snapshot, xid, &state.transactions)
                {
                    super::locks::check_concurrent_update(state, version, xid, snapshot)?;
                }
                pending.push(super::locks::RequiredRowLock {
                    key: origin.key,
                    mode: lock.mode,
                    mutation_candidate: Some(super::locks::MutationCandidate {
                        version_xmin: origin.version_xmin,
                        row: None,
                    }),
                });
                if state.row_locks.would_block(origin.key, xid, lock.mode) {
                    break;
                }
            }
            if !pending.is_empty() {
                context
                    .prepared_select_rows
                    .lock()
                    .expect("prepared SELECT rows mutex is poisoned")
                    .push(prepared);
                context.request_row_lock_recheck_with_locks(pending);
                return Err(PgError::create(SqlState::InternalError, LOCK_PENDING));
            }
            prepared.next += 1;
            if !skip {
                prepared.selected.push(row.clone());
            }
        }
        complete = (prepared.source_complete && prepared.next >= prepared.rows.len())
            || query_limit
                .is_some_and(|limit| prepared.selected.len() >= offset.saturating_add(limit));
        rows = prepared.selected.clone();
        if !complete {
            context
                .prepared_select_rows
                .lock()
                .expect("prepared SELECT rows mutex is poisoned")
                .push(prepared);
        }
    }
    let rows = finalize_select_rows(rows, &order_specs, &distinct, limit, offset)?;
    let (values, origins): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .map(|row| {
            let mut origins = row.origins;
            if let Some(source) = row.deferred_source {
                let projection = std::sync::Arc::new(DerivedProjection {
                    select: select.as_ref().clone(),
                    source: super::from::SourceRow {
                        values: source,
                        origins: origins.clone(),
                    },
                });
                for origin in &mut origins {
                    origin.projection = Some(projection.clone());
                }
            }
            (row.values, origins)
        })
        .unzip();
    let values_len = values.len();
    let output = QueryOutput {
        result: QueryResult {
            columns,
            rows: values,
        },
        origins,
        complete: complete || maximum_rows.is_some_and(|maximum| values_len < maximum),
    };
    if let Some((occurrence, sql)) = cache_key {
        let locks = context
            .select_row_locks
            .lock()
            .expect("select locks mutex is poisoned")[lock_start..]
            .to_vec();
        let mut cached = context
            .prepared_lock_queries
            .lock()
            .expect("prepared locks mutex is poisoned");
        cached.retain(|entry| entry.occurrence != occurrence || entry.sql != sql);
        cached.push(PreparedLockQuery {
            occurrence,
            sql,
            output: output.clone(),
            locks,
        });
    }
    Ok(output)
}

pub(in crate::executor) fn simplify_exists_query(
    state: &DatabaseState,
    query: &mut ast::Query,
    context: &StatementContext,
) -> Result<()> {
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        return Ok(());
    };
    if !query.locks.is_empty()
        || query.fetch.is_some()
        || query.with.as_ref().is_some_and(|with| {
            with.cte_tables.iter().any(|cte| {
                matches!(
                    cte.query.body.as_ref(),
                    ast::SetExpr::Insert(_) | ast::SetExpr::Update(_) | ast::SetExpr::Delete(_)
                )
            })
        })
        || select.having.is_some()
        || contains_query_aggregate(query)
    {
        return Ok(());
    }
    let ast::GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
        return Ok(());
    };
    if !modifiers.is_empty()
        || group_by.iter().any(|expr| {
            matches!(
                expr,
                ast::Expr::GroupingSets(_) | ast::Expr::Cube(_) | ast::Expr::Rollup(_)
            )
        })
    {
        return Ok(());
    }
    let mut has_special_function = false;
    let _ = ast::visit_expressions(&select.projection, |expr| {
        if let ast::Expr::Function(function) = expr {
            has_special_function |= function.over.is_some()
                || super::normalize_unqualified_object_name(&function.name)
                    .is_ok_and(|name| matches!(name.as_str(), "generate_series" | "unnest"));
        }
        std::ops::ControlFlow::<()>::Continue(())
    });
    if has_special_function {
        return Ok(());
    }
    let analysis = super::ctes::inline_query_ctes(query, &state.catalog, Some(state), true)?;
    let ast::SetExpr::Select(select) = analysis.body.as_ref() else {
        unreachable!()
    };
    let ast::GroupByExpr::Expressions(group_by, _) = &select.group_by else {
        unreachable!()
    };
    let scope = bind_select_scope(state, select)?;
    validate_select_predicates(state, select, &scope)?;
    let (projections, columns) = build_projection_plan(state, &select.projection, &scope)?;
    let order_specs = resolve_order_specs(state, &analysis, &projections, &columns, &scope)?;
    let distinct =
        resolve_distinct_plan(state, select, &projections, &columns, &order_specs, &scope)?;
    resolve_grouping_plan(
        state,
        select,
        group_by,
        &projections,
        &columns,
        &order_specs,
        &distinct,
        &scope,
    )?;
    if let Some(clause) = &query.limit_clause {
        match clause {
            ast::LimitClause::LimitOffset {
                limit,
                offset: None,
                limit_by,
            } if limit_by.is_empty() => {
                if let Some(limit) = limit {
                    let mut constant = true;
                    let _ = ast::visit_expressions(limit, |expr| {
                        constant &= match expr {
                            ast::Expr::Function(function) => {
                                super::normalize_unqualified_object_name(&function.name).is_ok_and(
                                    |name| {
                                        matches!(
                                            name.as_str(),
                                            "abs"
                                                | "floor"
                                                | "ceil"
                                                | "ceiling"
                                                | "round"
                                                | "trunc"
                                                | "length"
                                                | "char_length"
                                                | "octet_length"
                                                | "btrim"
                                                | "regexp_like"
                                                | "to_timestamp"
                                                | "jsonb_typeof"
                                                | "jsonb_array_length"
                                                | "lower"
                                                | "upper"
                                                | "coalesce"
                                                | "nullif"
                                                | "greatest"
                                                | "least"
                                        )
                                    },
                                )
                            }
                            ast::Expr::Subquery(_)
                            | ast::Expr::Exists { .. }
                            | ast::Expr::InSubquery { .. }
                            | ast::Expr::CompoundIdentifier(_) => false,
                            ast::Expr::Identifier(identifier) => {
                                identifier.value.eq_ignore_ascii_case("all")
                            }
                            ast::Expr::Value(value) => {
                                !matches!(value.value, ast::Value::Placeholder(_))
                            }
                            _ => true,
                        };
                        std::ops::ControlFlow::<()>::Continue(())
                    });
                    if !constant
                        || limits::evaluate_row_count(
                            limit,
                            limits::RowCountClause::Limit,
                            context,
                        )? == Some(0)
                    {
                        return Ok(());
                    }
                }
            }
            _ => return Ok(()),
        }
    }
    let ast::SetExpr::Select(select) = query.body.as_mut() else {
        unreachable!()
    };
    select.projection = vec![ast::SelectItem::UnnamedExpr(ast::Expr::Value(
        ast::Value::Number("1".into(), false).with_empty_span(),
    ))];
    select.group_by = ast::GroupByExpr::Expressions(Vec::new(), Vec::new());
    select.distinct = None;
    query.order_by = None;
    query.limit_clause = None;
    Ok(())
}
