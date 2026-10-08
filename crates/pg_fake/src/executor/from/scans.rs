use crate::{
    catalog::TableSchema,
    coercion::CastContext,
    error::{Result, reject_unsupported},
    executor::{
        DatabaseState, StatementContext,
        expressions::{evaluate, evaluate_and_coerce, resolve_operator_type},
        normalize_relation_name,
        scope::{BoundScope, RowScope},
    },
    serializable::Access,
    storage::{RowId, RowVersion, Table},
    txn::{Snapshot, Xid, find_visible_version},
    value::Value,
};
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn visit_table_factor_rows(
    state: &DatabaseState,
    factor: &ast::TableFactor,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    selection: Option<&ast::Expr>,
    start: usize,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let ast::TableFactor::Table {
        name: table_name,
        args,
        ..
    } = factor
    else {
        unreachable!("streamable source is a table");
    };
    if args.is_some() {
        return reject_unsupported("table functions are not implemented");
    }
    if let Some(id) = crate::executor::ctes::cte_row_source_id(table_name) {
        let source = context
            .get_cte_row_source(id)
            .expect("materialized CTE row source was registered");
        let mut filters = Vec::new();
        if let Some(selection) = selection {
            collect_pushdown_filters(
                selection,
                scope,
                start,
                start + source.columns.len(),
                &mut filters,
            );
        }
        let mut row = vec![Value::Null; scope.columns.len()];
        for values in &source.rows {
            row[start..start + values.len()].clone_from_slice(values);
            let passes = filters.iter().try_fold(true, |passes, filter| {
                if !passes {
                    return Ok(false);
                }
                Ok(matches!(
                    evaluate(filter, RowScope::Bound(scope), &row, context)?,
                    Value::Bool(true)
                ))
            })?;
            if passes {
                visit(&row)?;
            }
        }
        return Ok(());
    }
    let schema = state
        .catalog
        .require_named_table(&normalize_relation_name(table_name)?)?;
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
    let mut row = vec![Value::Null; scope.columns.len()];
    let frozen = context
        .query_source_state
        .lock()
        .expect("query source mutex is poisoned")
        .get(&schema.id)
        .cloned();
    let (table, transactions, source_snapshot) = frozen.as_ref().map_or_else(
        || {
            (
                state
                    .tables
                    .get(&schema.id)
                    .expect("catalog table must have storage"),
                &state.transactions,
                snapshot,
            )
        },
        |source| {
            (
                source.table.as_ref(),
                source.transactions.as_ref(),
                &context.source_snapshot,
            )
        },
    );
    for filter in &filters {
        let ast::Expr::BinaryOp {
            left,
            op: ast::BinaryOperator::Eq,
            right,
        } = filter
        else {
            continue;
        };
        let (column, value) = match (left.as_ref(), right.as_ref()) {
            (ast::Expr::Identifier(column), value) if is_point_lookup_value(value) => {
                (std::slice::from_ref(column), value)
            }
            (value, ast::Expr::Identifier(column)) if is_point_lookup_value(value) => {
                (std::slice::from_ref(column), value)
            }
            (ast::Expr::CompoundIdentifier(column), value) if is_point_lookup_value(value) => {
                (column.as_slice(), value)
            }
            (value, ast::Expr::CompoundIdentifier(column)) if is_point_lookup_value(value) => {
                (column.as_slice(), value)
            }
            _ => continue,
        };
        let Ok((slot, _)) = scope.resolve_column(column) else {
            continue;
        };
        if !(start..start + schema.columns.len()).contains(&slot) {
            continue;
        }
        let column = slot - start;
        let unique = table.has_unique_index(&[column]);
        if !unique && !table.has_nonunique_index(&[column]) {
            continue;
        }
        if resolve_operator_type(left, right, RowScope::Bound(scope))?
            != schema.columns[column].data_type.base
        {
            continue;
        }
        let value = evaluate_and_coerce(
            value,
            schema.columns[column].data_type.base,
            CastContext::Implicit,
            RowScope::Bound(scope),
            &row,
            context,
        )?;
        let mut visit_indexed_row = |row_id: RowId, indexed_version: &RowVersion| -> Result<()> {
            state.record_read(xid, Access::Row(schema.id, row_id));
            let indexed_row = &indexed_version.row;
            row[start..start + indexed_row.len()].clone_from_slice(indexed_row);
            let passes = filters.iter().try_fold(true, |passes, filter| {
                if !passes {
                    return Ok(false);
                }
                Ok(matches!(
                    evaluate(filter, RowScope::Bound(scope), &row, context)?,
                    Value::Bool(true)
                ))
            })?;
            if passes {
                visit(&row)?;
            }
            Ok(())
        };
        if unique {
            if state.tracks_serializable_reads(xid)
                && let Some(key) =
                    table.create_unique_read_key(&[column], std::slice::from_ref(&value))
            {
                state.record_read(xid, Access::Unique(schema.id, vec![column], key));
            }
            if let Some((row_id, indexed_version)) = table.find_unique_visible_version(
                &[column],
                &[value],
                source_snapshot,
                xid,
                transactions,
            ) {
                visit_indexed_row(row_id, indexed_version)?;
            }
        } else {
            state.record_read(xid, Access::Relation(schema.id));
            for (row_id, indexed_version) in table.find_nonunique_visible_versions(
                &[column],
                &[value],
                source_snapshot,
                xid,
                transactions,
            ) {
                visit_indexed_row(row_id, indexed_version)?;
            }
        }
        return Ok(());
    }
    state.record_read(xid, Access::Relation(schema.id));
    for (row_id, chain) in table.iterate_version_chains() {
        let Some(version) = find_visible_version(chain, source_snapshot, xid, transactions) else {
            continue;
        };
        state.record_read(xid, Access::Row(schema.id, row_id));
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
        if passes {
            visit(&row)?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn collect_pushdown_filters<'a>(
    expr: &'a ast::Expr,
    scope: &BoundScope,
    start: usize,
    end: usize,
    filters: &mut Vec<&'a ast::Expr>,
) {
    if let ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::And,
        right,
    } = expr
    {
        collect_pushdown_filters(left, scope, start, end, filters);
        collect_pushdown_filters(right, scope, start, end, filters);
        return;
    }
    if resolve_pushdown_column(expr, scope).is_some_and(|slot| (start..end).contains(&slot)) {
        filters.push(expr);
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_pushdown_column(expr: &ast::Expr, scope: &BoundScope) -> Option<usize> {
    if let ast::Expr::IsNull(inner) | ast::Expr::IsNotNull(inner) = expr {
        return match inner.as_ref() {
            ast::Expr::Identifier(column) => scope
                .resolve_column(std::slice::from_ref(column))
                .ok()
                .map(|(slot, _)| slot),
            ast::Expr::CompoundIdentifier(columns) => {
                scope.resolve_column(columns).ok().map(|(slot, _)| slot)
            }
            _ => None,
        };
    }
    let ast::Expr::BinaryOp { left, right, .. } = expr else {
        return None;
    };
    match (left.as_ref(), right.as_ref()) {
        (ast::Expr::Identifier(column), value) if is_point_lookup_value(value) => scope
            .resolve_column(std::slice::from_ref(column))
            .ok()
            .map(|(slot, _)| slot),
        (ast::Expr::CompoundIdentifier(columns), value) if is_point_lookup_value(value) => {
            scope.resolve_column(columns).ok().map(|(slot, _)| slot)
        }
        (value, ast::Expr::Identifier(column)) if is_point_lookup_value(value) => scope
            .resolve_column(std::slice::from_ref(column))
            .ok()
            .map(|(slot, _)| slot),
        (value, ast::Expr::CompoundIdentifier(columns)) if is_point_lookup_value(value) => {
            scope.resolve_column(columns).ok().map(|(slot, _)| slot)
        }
        _ => None,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn is_selection_fully_pushed(
    select: &ast::Select,
    scope: &BoundScope,
) -> bool {
    let [table] = select.from.as_slice() else {
        return false;
    };
    if !table.joins.is_empty()
        || !matches!(table.relation, ast::TableFactor::Table { args: None, .. })
    {
        return false;
    }
    select
        .selection
        .as_ref()
        .is_some_and(|selection| can_push_filter(selection, scope, 0, scope.columns.len()))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn can_push_filter(expr: &ast::Expr, scope: &BoundScope, start: usize, end: usize) -> bool {
    if let ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::And,
        right,
    } = expr
    {
        return can_push_filter(left, scope, start, end)
            && can_push_filter(right, scope, start, end);
    }
    resolve_pushdown_column(expr, scope).is_some_and(|slot| (start..end).contains(&slot))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_point_lookup_value(expr: &ast::Expr) -> bool {
    let mut expr = expr;
    loop {
        match expr {
            ast::Expr::Value(_) => return true,
            ast::Expr::Cast {
                kind: ast::CastKind::Cast | ast::CastKind::DoubleColon,
                expr: inner,
                format: None,
                ..
            } => expr = inner,
            _ => return false,
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn resolve_unique_point_lookup(
    table: &Table,
    schema: &TableSchema,
    selection: Option<&ast::Expr>,
    scope: RowScope<'_>,
    context: &StatementContext,
) -> Result<Option<(usize, Value)>> {
    let Some(ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::Eq,
        right,
    }) = selection
    else {
        return Ok(None);
    };
    let (column, value) = match (left.as_ref(), right.as_ref()) {
        (ast::Expr::Identifier(column), value) if is_point_lookup_value(value) => {
            (std::slice::from_ref(column), value)
        }
        (value, ast::Expr::Identifier(column)) if is_point_lookup_value(value) => {
            (std::slice::from_ref(column), value)
        }
        (ast::Expr::CompoundIdentifier(column), value) if is_point_lookup_value(value) => {
            (column.as_slice(), value)
        }
        (value, ast::Expr::CompoundIdentifier(column)) if is_point_lookup_value(value) => {
            (column.as_slice(), value)
        }
        _ => return Ok(None),
    };
    let Ok((column, _)) = scope.resolve_column(column) else {
        return Ok(None);
    };
    if column >= schema.columns.len()
        || !table.has_unique_index(&[column])
        || resolve_operator_type(left, right, scope)? != schema.columns[column].data_type.base
    {
        return Ok(None);
    }
    let value = evaluate_and_coerce(
        value,
        schema.columns[column].data_type.base,
        CastContext::Implicit,
        scope,
        &[],
        context,
    )?;
    Ok(Some((column, value)))
}
