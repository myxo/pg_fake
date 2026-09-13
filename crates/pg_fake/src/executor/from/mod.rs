use crate::{
    StatementResult,
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
    txn::{Snapshot, Xid, find_visible_version},
    value::{PgType, Value},
};
use sqlparser::ast;

mod joins;
mod scans;

use joins::{can_stream_join, materialize_table_with_joins_rows, visit_streamed_join_rows};
use scans::collect_pushdown_filters;
pub(super) use scans::{is_selection_fully_pushed, resolve_unique_point_lookup};

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
) -> Result<Vec<Vec<Value>>> {
    if from.is_empty() {
        return Ok(vec![Vec::new()]);
    }
    let mut next_slot = start_slot;
    let mut rows = vec![vec![Value::Null; scope.columns.len()]];
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
            &vec![Value::Null; scope.columns.len()],
        )?;
        rows = rows
            .into_iter()
            .flat_map(|left| {
                source.iter().map(move |right| {
                    left.iter()
                        .zip(right)
                        .map(|(left, right)| {
                            if left.is_null() {
                                right.clone()
                            } else {
                                left.clone()
                            }
                        })
                        .collect()
                })
            })
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
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    if crate::executor::lateral::skips_lateral_rows(selection, context) {
        return Ok(());
    }
    if let [table] = select.from.as_slice()
        && can_stream_join(table)
    {
        return visit_streamed_join_rows(
            state, table, scope, xid, snapshot, context, selection, visit,
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
        visit(&row)?;
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
    prefix: &[Value],
) -> Result<Vec<Vec<Value>>> {
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
                prefix,
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
                let mut row = prefix.to_vec();
                row[start..*next_slot].clone_from_slice(&values);
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
            (bound, slots) = bind_lateral_query(&state.catalog, subquery, &outer, prefix)?;
            correlated = !slots.is_empty();
            &bound
        } else {
            subquery.as_ref()
        };
        let InitplanKey::Scalar(projection) = InitplanKey::create_scalar(query) else {
            unreachable!("scalar key")
        };
        let initplan = InitplanKey::DerivedCte(projection);
        let cached = context
            .lateral_initplans
            .lock()
            .expect("lateral initplans mutex is poisoned")
            .get_result(&initplan)
            .or_else(|| {
                (!correlated)
                    .then(|| context.get_prepared_subquery_result(query))
                    .flatten()
            });
        let result = if let Some(result) = cached {
            result
        } else {
            let mut invocation = context.clone();
            invocation.lateral_invocation |= *lateral;
            if correlated {
                invocation.prepared_subquery_results = Default::default();
                invocation.prepared_cte_results = Default::default();
            }
            let StatementResult::Query(result) =
                execute_query(state, query, xid, snapshot, &invocation)?
            else {
                unreachable!("derived query execution returns query rows");
            };
            if !correlated {
                context.set_prepared_subquery_result(query, result.clone());
            }
            context
                .lateral_initplans
                .lock()
                .expect("lateral initplans mutex is poisoned")
                .set_result(&initplan, result.clone());
            result
        };
        let start = *next_slot;
        *next_slot += result.columns.len();
        return Ok(result
            .rows
            .into_iter()
            .map(|values| {
                let mut row = prefix.to_vec();
                row[start..start + values.len()].clone_from_slice(&values);
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
    let schema = state
        .catalog
        .require_named_table(&normalize_relation_name(table_name)?)?;
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
    state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .iterate_version_chains()
        .filter_map(|(_, chain)| find_visible_version(chain, snapshot, xid, &state.transactions))
        .map(|version| {
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
            Ok(passes.then_some(row))
        })
        .collect::<Result<Vec<_>>>()
        .map(|rows| rows.into_iter().flatten().collect())
}
