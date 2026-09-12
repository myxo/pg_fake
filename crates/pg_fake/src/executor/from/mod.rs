use crate::{
    StatementResult,
    coercion::{self, CastContext},
    error::{Result, reject_unsupported},
    executor::{
        DatabaseState, StatementContext,
        expressions::{evaluate, extract_unknown_string_literal},
        json, normalize_relation_name,
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
pub(super) use scans::is_selection_fully_pushed;

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
        let functions = json::contains_json_expansion(&table.relation)
            || table
                .joins
                .iter()
                .any(|join| json::contains_json_expansion(&join.relation));
        if functions {
            let start = next_slot;
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
            coercion::coerce_unknown(text, PgType::create(base), CastContext::Implicit)?
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
            coercion::coerce(value, source, PgType::create(base), CastContext::Implicit)?
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
        lateral,
        subquery,
        alias: Some(_),
        ..
    } = factor
    {
        if *lateral {
            return reject_unsupported("LATERAL derived tables are not implemented");
        }
        let StatementResult::Query(result) =
            execute_query(state, subquery, xid, snapshot, context)?
        else {
            unreachable!("derived query execution returns query rows");
        };
        let start = *next_slot;
        *next_slot += result.columns.len();
        return Ok(result
            .rows
            .into_iter()
            .map(|values| {
                let mut row = vec![Value::Null; scope.columns.len()];
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
