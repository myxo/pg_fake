use super::{
    SelectRow,
    distinct::{DistinctPlan, evaluate_distinct_keys},
    expressions::infer_query_expression_type,
    ordering::{
        OrderKey, RowOrderSpec, evaluate_order_keys, retain_top_ordered_row, sort_ordered_rows,
    },
    projection::{ProjectionSource, evaluate_projection_value, evaluate_projection_values},
};
use crate::{
    coercion::CastContext,
    error::{PgError, Result, SqlState},
    executor::{
        DatabaseState, StatementContext,
        equality::create_equality_key,
        expressions::{evaluate_and_coerce, is_null_literal},
        from::{is_selection_fully_pushed, visit_query_source_rows},
        normalize_relation_name,
        scope::{BoundScope, RowScope, bind_select_scope, try_resolve_column_reference},
        subqueries::evaluate_query_expression,
    },
    txn::{Snapshot, Xid, find_visible_version},
    value::{BaseType, Value},
};
use sqlparser::ast::{self, Spanned as _};

const SOURCE_ROW_LIMIT_REACHED: &str = "pg_fake source row limit reached";

#[derive(Clone)]
pub(crate) struct PreparedPlainRows {
    occurrence: sqlparser::tokenizer::Span,
    sql: String,
    visited: usize,
    rows: Vec<SelectRow>,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn validate_select_predicates(
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
pub(super) fn evaluate_where_clause(
    state: &DatabaseState,
    selection: Option<&ast::Expr>,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
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
pub(super) fn execute_plain_select_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    top_k: Option<usize>,
    locking: bool,
) -> Result<Vec<SelectRow>> {
    if !locking
        && !context.retain_row_origins
        && let Some(rows) = execute_correlated_exists_rows(
            state,
            select,
            scope,
            projections,
            order_specs,
            distinct,
            xid,
            snapshot,
            context,
        )
    {
        return rows;
    }
    if !locking
        && !context.retain_row_origins
        && let Some(rows) = execute_any_membership_rows(
            state,
            select,
            scope,
            projections,
            order_specs,
            distinct,
            xid,
            snapshot,
            context,
        )
    {
        return rows;
    }
    let occurrence = select.span();
    let sql = format!("{:?} {select}", context.query_invocation);
    let mut cached = context
        .prepared_plain_rows
        .lock()
        .expect("prepared source mutex is poisoned");
    let prepared = cached
        .iter()
        .position(|cached| cached.occurrence == occurrence && cached.sql == sql)
        .map(|index| cached.remove(index));
    drop(cached);
    let (mut visited, mut rows) = prepared.map_or((0, Vec::new()), |prepared| {
        (prepared.visited, prepared.rows)
    });
    let already_visited = visited;
    let mut seen = 0;
    let defer_projection = locking
        || context.retain_row_origins
        || (!order_specs.is_empty() && matches!(distinct, DistinctPlan::None));
    let remaining_selection = if is_selection_fully_pushed(select, scope) {
        None
    } else {
        select.selection.as_ref()
    };
    let result = if order_specs.is_empty() && top_k.is_some_and(|top_k| rows.len() >= top_k) {
        Ok(())
    } else {
        visit_query_source_rows(
            state,
            select,
            scope,
            xid,
            snapshot,
            context,
            select.selection.as_ref(),
            &mut |row, origins| {
                if seen < already_visited {
                    seen += 1;
                    return Ok(());
                }
                seen += 1;
                if !evaluate_where_clause(
                    state,
                    remaining_selection,
                    scope,
                    row,
                    xid,
                    snapshot,
                    context,
                )? {
                    visited += 1;
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
                            origins: origins.to_vec(),
                            values,
                            keys,
                            distinct_keys: Vec::new(),
                            deferred_source: Some(row.to_vec()),
                            evaluated_projections: Some(evaluated),
                        },
                        top_k,
                        order_specs,
                    );
                    visited += 1;
                    if order_specs.is_empty() && top_k.is_some_and(|top_k| rows.len() >= top_k) {
                        return Err(PgError::create(
                            SqlState::InternalError,
                            SOURCE_ROW_LIMIT_REACHED,
                        ));
                    }
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
                        origins: origins.to_vec(),
                        values,
                        keys,
                        distinct_keys,
                        deferred_source: None,
                        evaluated_projections: None,
                    },
                    top_k,
                    order_specs,
                );
                visited += 1;
                if order_specs.is_empty() && top_k.is_some_and(|top_k| rows.len() >= top_k) {
                    return Err(PgError::create(
                        SqlState::InternalError,
                        SOURCE_ROW_LIMIT_REACHED,
                    ));
                }
                Ok(())
            },
        )
    };
    if let Err(error) = result
        && (error.sqlstate != SqlState::InternalError || error.message != SOURCE_ROW_LIMIT_REACHED)
    {
        if context.capture_lock_queries {
            context
                .prepared_plain_rows
                .lock()
                .expect("prepared source mutex is poisoned")
                .push(PreparedPlainRows {
                    occurrence,
                    sql,
                    visited,
                    rows,
                });
        }
        return Err(error);
    }
    let projected = (|| {
        if defer_projection && !locking {
            sort_ordered_rows(&mut rows, order_specs);
            for row in &mut rows {
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
                            state, projection, scope, source, None, xid, snapshot, context,
                        )?;
                        evaluated[index] = true;
                    }
                }
            }
        }
        Ok(())
    })();
    if context.capture_lock_queries {
        context
            .prepared_plain_rows
            .lock()
            .expect("prepared source mutex is poisoned")
            .push(PreparedPlainRows {
                occurrence,
                sql,
                visited,
                rows: rows.clone(),
            });
    }
    projected?;
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
    context: &StatementContext,
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
    state.record_read(xid, crate::serializable::Access::Relation(schema.id));
    for (row_id, chain) in state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .iterate_version_chains()
    {
        if let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions) {
            state.record_read(xid, crate::serializable::Access::Row(schema.id, row_id));
            if let Some(key) = create_equality_key(&version.row[inner_slot]) {
                matches.insert(key);
            }
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
        &mut |row, origins| {
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
                origins: origins.to_vec(),
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
    context: &StatementContext,
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
        &mut |row, origins| {
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
                origins: origins.to_vec(),
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
