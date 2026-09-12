use crate::{
    coercion::CastContext,
    error::{Result, reject_unsupported},
    executor::{
        DatabaseState, StatementExecutionContext,
        expressions::{evaluate, evaluate_and_coerce, resolve_operator_type},
        normalize_relation_name,
        scope::{BoundScope, RowScope},
    },
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
    context: &StatementExecutionContext,
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
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
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
        if !table.has_unique_index(&[column])
            || resolve_operator_type(left, right, RowScope::Bound(scope))?
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
        let Some(indexed_row) =
            table.find_unique_visible_row(&[column], &[value], snapshot, xid, &state.transactions)
        else {
            return Ok(());
        };
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
        return Ok(());
    }
    for (_, chain) in table.iterate_version_chains() {
        let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions) else {
            continue;
        };
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
    matches!(expr, ast::Expr::Value(_))
        || matches!(
            expr,
            ast::Expr::Cast {
                kind: ast::CastKind::Cast | ast::CastKind::DoubleColon,
                expr,
                format: None,
                ..
            } if matches!(expr.as_ref(), ast::Expr::Value(_))
        )
}
