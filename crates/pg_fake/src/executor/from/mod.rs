use crate::{
    coercion::{self, CastContext},
    error::{Result, reject_unsupported},
    executor::{
        DatabaseState, StatementContext,
        expressions::{evaluate, extract_unknown_string_literal},
        json,
        lateral::{InitplanKey, bind_lateral_query, contains_lateral_source},
        normalize_relation_name,
        query::execute_query,
        scope::{self, BoundScope, RowScope},
        subqueries::evaluate_query_expression,
    },
    txn::{RowLockKey, Snapshot, Xid, find_visible_version},
    value::{PgType, Value},
};
use sqlparser::ast::{self, Spanned as _};
use sqlparser::tokenizer::Span;

mod joins;
mod scans;
mod streaming;
mod subqueries;
pub(super) use streaming::recheck_join_conditions;

type RowConsumer<'a> = dyn FnMut(&[Value], &[RowOrigin]) -> Result<()> + 'a;

use joins::{can_stream_join, materialize_table_with_joins_rows, visit_streamed_join_rows};
use scans::collect_pushdown_filters;
pub(super) use scans::{is_selection_fully_pushed, resolve_unique_point_lookup};

#[derive(Clone)]
pub(crate) struct RowOrigin {
    pub(crate) source: Span,
    pub(crate) key: RowLockKey,
    pub(crate) version_xmin: Xid,
    pub(crate) start: Option<usize>,
    pub(crate) projection: Option<std::sync::Arc<super::query::DerivedProjection>>,
}

#[derive(Clone)]
pub(super) struct SourceRow {
    pub(super) values: Vec<Value>,
    pub(super) origins: Vec<RowOrigin>,
}

impl SourceRow {
    fn create(values: Vec<Value>) -> Self {
        Self {
            values,
            origins: Vec::new(),
        }
    }

    fn combine(&self, right: &Self) -> Self {
        Self {
            values: self
                .values
                .iter()
                .zip(&right.values)
                .map(|(left, right)| {
                    if left.is_null() {
                        right.clone()
                    } else {
                        left.clone()
                    }
                })
                .collect(),
            origins: self.origins.iter().chain(&right.origins).cloned().collect(),
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn materialize_from_rows(
    state: &DatabaseState,
    from: &[ast::TableWithJoins],
    scope: &BoundScope,
    start_slot: usize,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    selection: Option<&ast::Expr>,
) -> Result<Vec<SourceRow>> {
    if from.is_empty() {
        return Ok(vec![SourceRow::create(Vec::new())]);
    }
    let mut next_slot = start_slot;
    let mut rows = vec![SourceRow::create(vec![Value::Null; scope.columns.len()])];
    for table in from {
        let functions = contains_lateral_source(&table.relation)
            || table
                .joins
                .iter()
                .any(|join| contains_lateral_source(&join.relation));
        if functions {
            let start = next_slot;
            let mut bound = BoundScope {
                columns: scope.columns[..start].to_vec(),
            };
            scope::bind_table_with_joins(&state.catalog, table, &mut bound)?;
            next_slot = bound.columns.len();
            let mut expanded = Vec::new();
            for prefix in &rows {
                let mut slot = start;
                expanded.extend(materialize_table_with_joins_rows(
                    state, table, scope, xid, snapshot, context, selection, &mut slot, prefix,
                )?);
                next_slot = slot;
            }
            rows = expanded;
            continue;
        }
        let source = materialize_table_with_joins_rows(
            state,
            table,
            scope,
            xid,
            snapshot,
            context,
            selection,
            &mut next_slot,
            &SourceRow::create(vec![Value::Null; scope.columns.len()]),
        )?;
        rows = rows
            .into_iter()
            .flat_map(|left| source.iter().map(move |right| left.combine(right)))
            .collect();
    }
    Ok(rows)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn visit_query_source_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    selection: Option<&ast::Expr>,
    visit: &mut RowConsumer<'_>,
) -> Result<()> {
    if crate::executor::lateral::skips_lateral_rows(selection, context) {
        return Ok(());
    }
    if context.capture_lock_queries {
        return streaming::visit_demand_source_rows(
            state, select, scope, xid, snapshot, context, selection, visit,
        );
    }
    if let [table] = select.from.as_slice()
        && can_stream_join(table)
        && std::iter::once(&table.relation)
            .chain(table.joins.iter().map(|join| &join.relation))
            .all(|factor| {
                let ast::TableFactor::Table { name, .. } = factor else {
                    return true;
                };
                normalize_relation_name(name).ok().is_none_or(|name| {
                    super::describe_visible_system_relation(&state.catalog, &name).is_none()
                })
            })
        && !context.retain_row_origins
    {
        return visit_streamed_join_rows(
            state,
            table,
            scope,
            xid,
            snapshot,
            context,
            selection,
            &mut |row| visit(row, &[]),
        );
    }
    for row in materialize_from_rows(
        state,
        &select.from,
        scope,
        0,
        xid,
        snapshot,
        context,
        selection,
    )? {
        visit(&row.values, &row.origins)?;
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materialize_table_factor_rows(
    state: &DatabaseState,
    factor: &ast::TableFactor,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    selection: Option<&ast::Expr>,
    next_slot: &mut usize,
    prefix: &SourceRow,
) -> Result<Vec<SourceRow>> {
    if let Some(json::JsonTableFunction {
        name,
        argument,
        ordinality,
        ..
    }) = json::extract_json_table_function(factor)?
    {
        let start = *next_slot;
        *next_slot += json::describe_json_expansion(&name, ordinality).len();
        let base = json::resolve_json_function_arguments(&name).expect("JSON expansion")[0];
        let argument_scope = BoundScope {
            columns: scope.columns[..start].to_vec(),
        };
        let value = if let Some(text) = extract_unknown_string_literal(argument) {
            coercion::coerce_unknown(
                text,
                PgType::create(base),
                CastContext::Implicit,
                &context.timezone,
            )?
        } else {
            let source =
                scope::infer_expression_data_type(&state.catalog, argument, &argument_scope)?.base;
            let value = evaluate_query_expression(
                state,
                argument,
                &argument_scope,
                &prefix.values,
                xid,
                snapshot,
                context,
            )?;
            coercion::coerce(
                value,
                source,
                PgType::create(base),
                CastContext::Implicit,
                &context.timezone,
            )?
        };
        return Ok(json::evaluate_json_expansion(&name, value, ordinality)?
            .into_iter()
            .map(|values| {
                let mut row = prefix.clone();
                row.values[start..*next_slot].clone_from_slice(&values);
                row
            })
            .collect());
    }
    if let ast::TableFactor::NestedJoin {
        table_with_joins, ..
    } = factor
    {
        let mut nested_scope = BoundScope {
            columns: scope.columns[..*next_slot].to_vec(),
        };
        scope::bind_table_with_joins(&state.catalog, table_with_joins, &mut nested_scope)?;
        let end = nested_scope.columns.len();
        nested_scope
            .columns
            .extend_from_slice(&scope.columns[end..]);
        return materialize_table_with_joins_rows(
            state,
            table_with_joins,
            &nested_scope,
            xid,
            snapshot,
            context,
            selection,
            next_slot,
            prefix,
        );
    }
    if let ast::TableFactor::Derived {
        lateral, subquery, ..
    } = factor
    {
        let bound;
        let mut correlated = false;
        let query = if *lateral {
            let outer = BoundScope {
                columns: scope.columns[..*next_slot].to_vec(),
            };
            let slots;
            (bound, slots) = bind_lateral_query(&state.catalog, subquery, &outer, &prefix.values)?;
            correlated = !slots.is_empty();
            &bound
        } else {
            subquery.as_ref()
        };
        let mut context = context.clone();
        if !correlated {
            context.query_invocation.clear();
        }
        let context = &context;
        let is_cte = context
            .cte_query_barriers
            .lock()
            .expect("locking CTE mutex is poisoned")
            .contains(query);
        let inherited = (!is_cte && super::query::requires_nested_locking(query))
            .then(|| {
                context
                    .source_row_locks
                    .iter()
                    .find(|(source, _)| *source == factor.span())
                    .map(|(_, lock)| *lock)
            })
            .flatten();
        let filtered = if is_cte {
            None
        } else {
            subqueries::push_derived_filters(
                state,
                query,
                scope,
                *next_slot,
                selection,
                inherited.is_some(),
            )?
        };
        let query = filtered.as_ref().unwrap_or(query);
        let InitplanKey::Scalar(projection) = InitplanKey::create_scalar(query) else {
            unreachable!("scalar key")
        };
        let initplan = InitplanKey::DerivedCte(projection);
        let cached = (!context.capture_lock_queries && inherited.is_none())
            .then(|| {
                context
                    .lateral_initplans
                    .lock()
                    .expect("lateral initplans mutex is poisoned")
                    .get_result(&initplan)
                    .map(super::query::QueryOutput::create)
                    .or_else(|| {
                        (!correlated)
                            .then(|| context.get_prepared_subquery_result(query))
                            .flatten()
                    })
            })
            .flatten();
        let result = if let Some(result) = cached {
            result
        } else {
            let mut invocation = context.clone();
            invocation.lateral_invocation |= *lateral;
            invocation.inherited_row_lock = inherited;
            if correlated {
                invocation.prepared_subquery_results = Default::default();

                invocation.prepared_cte_results = Default::default();
            }
            let result = execute_query(state, query, xid, snapshot, &invocation)?;
            if !correlated && !context.capture_lock_queries {
                context.set_prepared_subquery_result(query, result.clone());
            }
            context
                .lateral_initplans
                .lock()
                .expect("lateral initplans mutex is poisoned")
                .set_result(&initplan, result.result.clone());
            result
        };
        let start = *next_slot;
        *next_slot += result.result.columns.len();
        return Ok(result
            .result
            .rows
            .into_iter()
            .zip(result.origins)
            .map(|(values, origins)| {
                let mut row = prefix.clone();
                row.values[start..start + values.len()].clone_from_slice(&values);
                row.origins.extend(origins.into_iter().map(|mut origin| {
                    origin.source = factor.span();
                    origin.start = Some(start);
                    origin
                }));
                row
            })
            .collect());
    }
    let ast::TableFactor::Table {
        name: table_name,
        args,
        ..
    } = factor
    else {
        return reject_unsupported("FROM source is not implemented");
    };
    if args.is_some() {
        return reject_unsupported("table functions are not implemented");
    }
    let relation_name = normalize_relation_name(table_name)?;
    if let Some(rows) = super::materialize_system_relation(&state.catalog, &relation_name) {
        let column_count = super::describe_visible_system_relation(&state.catalog, &relation_name)
            .expect("materialized system relation has columns")
            .len();
        let start = *next_slot;
        *next_slot += column_count;
        let mut filters = Vec::new();
        if let Some(selection) = selection {
            collect_pushdown_filters(selection, scope, start, *next_slot, &mut filters);
        }
        return rows
            .into_iter()
            .map(|values| {
                let mut row = vec![Value::Null; scope.columns.len()];
                row[start..start + values.len()].clone_from_slice(&values);
                let passes = filters.iter().try_fold(true, |passes, filter| {
                    if !passes {
                        return Ok(false);
                    }
                    Ok(matches!(
                        evaluate(filter, RowScope::Bound(scope), &row, context)?,
                        Value::Bool(true)
                    ))
                })?;
                Ok(passes.then_some(SourceRow::create(row)))
            })
            .collect::<Result<Vec<_>>>()
            .map(|rows| rows.into_iter().flatten().collect());
    }
    let schema = state.catalog.require_named_table(&relation_name)?;
    let start = *next_slot;
    *next_slot += schema.columns.len();
    let mut filters = Vec::new();
    if let Some(selection) = selection {
        collect_pushdown_filters(
            selection,
            scope,
            start,
            start + schema.columns.len(),
            &mut filters,
        );
    }
    let frozen = context
        .query_source_state
        .lock()
        .expect("query source mutex is poisoned")
        .clone();
    let source_state = frozen.as_deref().unwrap_or(state);
    let source_snapshot = if frozen.is_some() {
        &context.source_snapshot
    } else {
        snapshot
    };
    source_state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .iterate_version_chains()
        .filter_map(|(row_id, chain)| {
            find_visible_version(chain, source_snapshot, xid, &source_state.transactions)
                .map(|version| (row_id, version))
        })
        .map(|(row_id, version)| {
            let mut row = vec![Value::Null; scope.columns.len()];
            row[start..start + version.row.len()].clone_from_slice(&version.row);
            let passes = filters.iter().try_fold(true, |passes, filter| {
                if !passes {
                    return Ok(false);
                }
                Ok(matches!(
                    evaluate(filter, RowScope::Bound(scope), &row, context)?,
                    Value::Bool(true)
                ))
            })?;
            Ok(passes.then_some(SourceRow {
                values: row,
                origins: if context.retain_row_origins {
                    vec![RowOrigin {
                        source: factor.span(),
                        key: RowLockKey {
                            table_id: schema.id,
                            row_id,
                        },
                        version_xmin: version.xmin,
                        start: Some(start),
                        projection: None,
                    }]
                } else {
                    Vec::new()
                },
            }))
        })
        .collect::<Result<Vec<_>>>()
        .map(|rows| rows.into_iter().flatten().collect())
}
