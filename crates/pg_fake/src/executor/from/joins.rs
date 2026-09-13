use crate::{
    coercion::{self, CastContext},
    error::{Result, reject_unsupported},
    executor::{
        DatabaseState, StatementContext,
        equality::{EqualityKey, create_equality_key},
        lateral::contains_lateral_source,
        normalize_relation_name, normalize_unqualified_object_name,
        scope::{self, BoundScope, try_resolve_column_reference},
        subqueries::evaluate_query_expression,
    },
    txn::{Snapshot, Xid},
    value::{BaseType, PgType, Value},
};
use sqlparser::ast;

use super::{SourceRow, materialize_table_factor_rows, scans::visit_table_factor_rows};

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn can_stream_join(table: &ast::TableWithJoins) -> bool {
    matches!(table.relation, ast::TableFactor::Table { args: None, .. })
        && table.joins.iter().all(|join| {
            matches!(join.relation, ast::TableFactor::Table { args: None, .. })
                && matches!(
                    join.join_operator,
                    ast::JoinOperator::Join(_)
                        | ast::JoinOperator::Inner(_)
                        | ast::JoinOperator::CrossJoin(_)
                        | ast::JoinOperator::Left(_)
                        | ast::JoinOperator::LeftOuter(_)
                )
        })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn visit_streamed_join_rows(
    state: &DatabaseState,
    table: &ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    selection: Option<&ast::Expr>,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let mut starts = Vec::with_capacity(table.joins.len() + 1);
    let mut next_slot = 0;
    for factor in
        std::iter::once(&table.relation).chain(table.joins.iter().map(|join| &join.relation))
    {
        let ast::TableFactor::Table {
            name: table_name, ..
        } = factor
        else {
            unreachable!("streamable sources are tables");
        };
        starts.push(next_slot);
        next_slot += state
            .catalog
            .require_named_table(&normalize_relation_name(table_name)?)?
            .columns
            .len();
    }
    let hash_slots = table
        .joins
        .iter()
        .enumerate()
        .map(|(index, join)| {
            resolve_hash_join_slots(&join.join_operator, scope, starts[0], starts[index + 1])
        })
        .collect::<Option<Vec<_>>>();
    if let Some(hash_slots) = hash_slots
        && !hash_slots.is_empty()
    {
        return visit_hash_join_chain_rows(
            state,
            table,
            scope,
            xid,
            snapshot,
            context,
            selection,
            &starts,
            &hash_slots,
            visit,
        );
    }
    let mut right_sources = Vec::with_capacity(table.joins.len());
    for (index, join) in table.joins.iter().enumerate() {
        let mut rows = Vec::new();
        visit_table_factor_rows(
            state,
            &join.relation,
            scope,
            xid,
            snapshot,
            context,
            selection,
            starts[index + 1],
            &mut |row| {
                rows.push(row.to_vec());
                Ok(())
            },
        )?;
        right_sources.push(rows);
    }
    visit_table_factor_rows(
        state,
        &table.relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        starts[0],
        &mut |row| {
            visit_nested_loop_join_rows(
                state,
                table,
                scope,
                xid,
                snapshot,
                context,
                &starts,
                &right_sources,
                0,
                row,
                visit,
            )
        },
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_hash_join_slots(
    operator: &ast::JoinOperator,
    scope: &BoundScope,
    left_start: usize,
    right_start: usize,
) -> Option<(usize, usize, bool)> {
    let (ast::JoinOperator::Join(ast::JoinConstraint::On(expression))
    | ast::JoinOperator::Inner(ast::JoinConstraint::On(expression))
    | ast::JoinOperator::Left(ast::JoinConstraint::On(expression))
    | ast::JoinOperator::LeftOuter(ast::JoinConstraint::On(expression))) = operator
    else {
        return None;
    };
    let preserve_left = matches!(
        operator,
        ast::JoinOperator::Left(_) | ast::JoinOperator::LeftOuter(_)
    );
    let ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::Eq,
        right,
    } = expression
    else {
        return None;
    };
    let (left_slot, left_type) = try_resolve_column_reference(left, scope)?;
    let (right_slot, right_type) = try_resolve_column_reference(right, scope)?;
    if left_type.base != right_type.base
        || !matches!(
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
                | BaseType::Jsonb
        )
    {
        return None;
    }
    if (left_start..right_start).contains(&right_slot)
        && (right_start..scope.columns.len()).contains(&left_slot)
    {
        return Some((right_slot, left_slot, preserve_left));
    }
    ((left_start..right_start).contains(&left_slot)
        && (right_start..scope.columns.len()).contains(&right_slot))
    .then_some((left_slot, right_slot, preserve_left))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn visit_hash_join_chain_rows(
    state: &DatabaseState,
    table: &ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    selection: Option<&ast::Expr>,
    starts: &[usize],
    hash_slots: &[(usize, usize, bool)],
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let mut rows = Vec::new();
    let first_end = starts.get(1).copied().unwrap_or(scope.columns.len());
    visit_table_factor_rows(
        state,
        &table.relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        starts[0],
        &mut |row| {
            rows.push(row[..first_end].to_vec());
            Ok(())
        },
    )?;
    for (index, (left_slot, right_slot, preserve_left)) in hash_slots.iter().copied().enumerate() {
        let right_start = starts[index + 1];
        let right_end = starts
            .get(index + 2)
            .copied()
            .unwrap_or(scope.columns.len());
        let mut right_rows = std::collections::HashMap::<EqualityKey, Vec<Vec<Value>>>::new();
        visit_table_factor_rows(
            state,
            &table.joins[index].relation,
            scope,
            xid,
            snapshot,
            context,
            selection,
            right_start,
            &mut |row| {
                if let Some(key) = create_equality_key(&row[right_slot]) {
                    right_rows
                        .entry(key)
                        .or_default()
                        .push(row[right_start..right_end].to_vec());
                }
                Ok(())
            },
        )?;
        let mut joined = Vec::new();
        for left in rows {
            let matches =
                create_equality_key(&left[left_slot]).and_then(|key| right_rows.get(&key));
            if let Some(matches) = matches {
                joined.extend(matches.iter().map(|right| {
                    let mut row = left.clone();
                    row.extend_from_slice(right);
                    row
                }));
            } else if preserve_left {
                let mut row = left;
                row.resize(right_end, Value::Null);
                joined.push(row);
            }
        }
        rows = joined;
    }
    for row in rows {
        visit(&row)?;
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn visit_nested_loop_join_rows(
    state: &DatabaseState,
    table: &ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    starts: &[usize],
    right_sources: &[Vec<Vec<Value>>],
    index: usize,
    left: &[Value],
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let Some(join) = table.joins.get(index) else {
        return visit(left);
    };
    let mut matched_left = false;
    for right in &right_sources[index] {
        let row = left
            .iter()
            .zip(right)
            .map(|(left, right)| {
                if left.is_null() {
                    right.clone()
                } else {
                    left.clone()
                }
            })
            .collect::<Vec<_>>();
        if evaluate_join_condition(
            state,
            &join.join_operator,
            &row,
            scope,
            starts[0],
            starts[index + 1],
            xid,
            snapshot,
            context,
        )? {
            matched_left = true;
            visit_nested_loop_join_rows(
                state,
                table,
                scope,
                xid,
                snapshot,
                context,
                starts,
                right_sources,
                index + 1,
                &row,
                visit,
            )?;
        }
    }
    if !matched_left
        && matches!(
            join.join_operator,
            ast::JoinOperator::Left(_) | ast::JoinOperator::LeftOuter(_)
        )
    {
        visit_nested_loop_join_rows(
            state,
            table,
            scope,
            xid,
            snapshot,
            context,
            starts,
            right_sources,
            index + 1,
            left,
            visit,
        )?;
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn materialize_table_with_joins_rows(
    state: &DatabaseState,
    table: &ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    selection: Option<&ast::Expr>,
    next_slot: &mut usize,
    prefix: &SourceRow,
) -> Result<Vec<SourceRow>> {
    let left_start = *next_slot;
    let mut rows = materialize_table_factor_rows(
        state,
        &table.relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        next_slot,
        prefix,
    )?;
    for row in &mut rows {
        for origin in &prefix.origins {
            if !row
                .origins
                .iter()
                .any(|existing| existing.source == origin.source && existing.key == origin.key)
            {
                row.origins.push(origin.clone());
            }
        }
        for (value, prefix) in row.values.iter_mut().zip(&prefix.values) {
            if value.is_null() {
                *value = prefix.clone();
            }
        }
    }
    for join in &table.joins {
        let right_start = *next_slot;
        if contains_lateral_source(&join.relation)
            && !matches!(
                join.join_operator,
                ast::JoinOperator::Right(_)
                    | ast::JoinOperator::RightOuter(_)
                    | ast::JoinOperator::FullOuter(_)
            )
        {
            let mut bound = BoundScope {
                columns: scope.columns[..right_start].to_vec(),
            };
            scope::bind_table_factor(&state.catalog, &join.relation, &mut bound)?;
            *next_slot = bound.columns.len();
            let mut joined = Vec::new();
            for (index, left) in rows.into_iter().enumerate() {
                let mut context = context.clone();
                if context.capture_lock_queries {
                    context.query_invocation.push(index);
                }
                let context = &context;
                let mut matched = false;
                let mut slot = right_start;
                for row in materialize_table_factor_rows(
                    state,
                    &join.relation,
                    scope,
                    xid,
                    snapshot,
                    context,
                    selection,
                    &mut slot,
                    &left,
                )? {
                    if evaluate_join_condition(
                        state,
                        &join.join_operator,
                        &row.values,
                        scope,
                        left_start,
                        right_start,
                        xid,
                        snapshot,
                        context,
                    )? {
                        matched = true;
                        joined.push(row);
                    }
                }
                if !matched
                    && matches!(
                        join.join_operator,
                        ast::JoinOperator::Left(_) | ast::JoinOperator::LeftOuter(_)
                    )
                {
                    joined.push(left);
                }
            }
            rows = joined;
            continue;
        }
        let right_rows = materialize_table_factor_rows(
            state,
            &join.relation,
            scope,
            xid,
            snapshot,
            context,
            selection,
            next_slot,
            prefix,
        )?;
        let mut joined = Vec::new();
        let mut matched_right = vec![false; right_rows.len()];
        for left in &rows {
            let mut matched_left = false;
            for (index, right) in right_rows.iter().enumerate() {
                let row = left.combine(right);
                if evaluate_join_condition(
                    state,
                    &join.join_operator,
                    &row.values,
                    scope,
                    left_start,
                    right_start,
                    xid,
                    snapshot,
                    context,
                )? {
                    matched_left = true;
                    matched_right[index] = true;
                    joined.push(row);
                }
            }
            if !matched_left
                && matches!(
                    join.join_operator,
                    ast::JoinOperator::Left(_)
                        | ast::JoinOperator::LeftOuter(_)
                        | ast::JoinOperator::FullOuter(_)
                )
            {
                joined.push(left.clone());
            }
        }
        if matches!(
            join.join_operator,
            ast::JoinOperator::Right(_)
                | ast::JoinOperator::RightOuter(_)
                | ast::JoinOperator::FullOuter(_)
        ) {
            joined.extend(
                right_rows
                    .iter()
                    .zip(matched_right)
                    .filter_map(|(row, matched)| (!matched).then_some(row.clone())),
            );
        }
        rows = joined;
    }
    Ok(rows)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_join_condition(
    state: &DatabaseState,
    operator: &ast::JoinOperator,
    row: &[Value],
    scope: &BoundScope,
    left_start: usize,
    right_start: usize,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<bool> {
    let constraint = match operator {
        ast::JoinOperator::Join(constraint)
        | ast::JoinOperator::Inner(constraint)
        | ast::JoinOperator::CrossJoin(constraint)
        | ast::JoinOperator::Left(constraint)
        | ast::JoinOperator::LeftOuter(constraint)
        | ast::JoinOperator::Right(constraint)
        | ast::JoinOperator::RightOuter(constraint)
        | ast::JoinOperator::FullOuter(constraint) => constraint,
        _ => {
            return reject_unsupported("join type is not implemented");
        }
    };
    match constraint {
        ast::JoinConstraint::None => Ok(matches!(operator, ast::JoinOperator::CrossJoin(_))),
        ast::JoinConstraint::On(expression) => Ok(matches!(
            evaluate_query_expression(state, expression, scope, row, xid, snapshot, context,)?,
            Value::Bool(true)
        )),
        ast::JoinConstraint::Using(names) => evaluate_using_join_condition(
            names
                .iter()
                .map(normalize_unqualified_object_name)
                .collect::<Result<Vec<_>>>()?
                .as_slice(),
            row,
            scope,
            left_start,
            right_start,
        ),
        ast::JoinConstraint::Natural => {
            let names = scope.columns[left_start..right_start]
                .iter()
                .filter(|left| {
                    scope.columns[right_start..]
                        .iter()
                        .any(|right| right.name == left.name)
                })
                .map(|column| column.name.clone())
                .collect::<Vec<_>>();
            evaluate_using_join_condition(&names, row, scope, left_start, right_start)
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_using_join_condition(
    names: &[String],
    row: &[Value],
    scope: &BoundScope,
    left_start: usize,
    right_start: usize,
) -> Result<bool> {
    for name in names {
        let left = scope.columns[left_start..right_start]
            .iter()
            .find(|column| column.unqualified && column.name == *name)
            .expect("bound USING column must exist in left source");
        let right = scope.columns[right_start..]
            .iter()
            .find(|column| !column.unqualified && column.name == *name)
            .expect("bound USING column must exist in right source");
        let data_type = coercion::resolve_common_type(left.data_type.base, right.data_type.base)
            .expect("bound USING columns must have a common type");
        let left = coercion::coerce(
            left.merged
                .as_deref()
                .unwrap_or(std::slice::from_ref(&left.slot))
                .iter()
                .filter(|slot| **slot < right_start)
                .map(|slot| &row[*slot])
                .find(|v| !v.is_null())
                .cloned()
                .unwrap_or(Value::Null),
            left.data_type.base,
            PgType::create(data_type),
            CastContext::Implicit,
            "UTC",
        )?;
        let right = coercion::coerce(
            right
                .merged
                .as_deref()
                .unwrap_or(std::slice::from_ref(&right.slot))
                .iter()
                .filter(|slot| **slot >= right_start)
                .map(|slot| &row[*slot])
                .find(|v| !v.is_null())
                .cloned()
                .unwrap_or(Value::Null),
            right.data_type.base,
            PgType::create(data_type),
            CastContext::Implicit,
            "UTC",
        )?;
        if left.is_null() || right.is_null() || left != right {
            return Ok(false);
        }
    }
    Ok(true)
}
