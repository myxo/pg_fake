use crate::{
    ColumnMeta,
    catalog::TableId,
    database::DatabaseState,
    error::{PgError, Result, SqlState},
    serializable::Access,
    txn::{Snapshot, Xid, find_visible_version},
    value::{BaseType, Value},
};
use sqlparser::ast;
use std::{collections::HashMap, hash::Hash, time::Instant};

use super::{
    PreparedQueryPlan, StatementContext, normalize_relation_name,
    prepared::{PreparedExpression, bind_prepared_expression, evaluate_prepared_expression},
    query::describe_query_result_columns,
    scope::{bind_query_scope, try_resolve_column_reference},
};

#[derive(Debug, Clone)]
pub(crate) enum PreparedReadPlan {
    Query(PreparedQueryPlan),
    InnerJoin(PreparedInnerJoinPlan),
    UniqueInnerJoin(PreparedInnerJoinPlan),
    AdaptiveUniqueInnerJoin(PreparedInnerJoinPlan),
    DoubleFilteredInnerJoin(Box<PreparedDoubleFilteredInnerJoinPlan>),
    UnfilteredInnerJoin(PreparedUnfilteredInnerJoinPlan),
    UnfilteredLeftJoin(PreparedUnfilteredInnerJoinPlan),
    UnfilteredWideLeftJoin(PreparedUnfilteredInnerJoinPlan),
    UnfilteredJoinChain(PreparedUnfilteredJoinChainPlan),
}

impl PreparedReadPlan {
    pub(crate) fn columns(&self) -> &[ColumnMeta] {
        match self {
            Self::Query(plan) => plan.columns(),
            Self::InnerJoin(plan)
            | Self::UniqueInnerJoin(plan)
            | Self::AdaptiveUniqueInnerJoin(plan) => &plan.columns,
            Self::DoubleFilteredInnerJoin(plan) => &plan.join.columns,
            Self::UnfilteredInnerJoin(plan)
            | Self::UnfilteredLeftJoin(plan)
            | Self::UnfilteredWideLeftJoin(plan) => &plan.columns,
            Self::UnfilteredJoinChain(plan) => &plan.columns,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedUnfilteredInnerJoinPlan {
    left_table_id: TableId,
    right_table_id: TableId,
    left_width: usize,
    left_key: usize,
    right_key: usize,
    projection: Vec<usize>,
    columns: Vec<ColumnMeta>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedUnfilteredJoinChainPlan {
    table_ids: Vec<TableId>,
    joins: Vec<(usize, usize, usize, bool)>,
    projection: Vec<(usize, usize)>,
    columns: Vec<ColumnMeta>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedInnerJoinPlan {
    left_table_id: TableId,
    right_table_id: TableId,
    left_width: usize,
    left_key: usize,
    right_key: usize,
    filter_slot: usize,
    filter_value: PreparedExpression,
    projection: Vec<usize>,
    columns: Vec<ColumnMeta>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedDoubleFilteredInnerJoinPlan {
    join: PreparedInnerJoinPlan,
    right_filter_slot: usize,
    right_filter_value: PreparedExpression,
}

fn extract_integer_join_filter(
    expression: PreparedExpression,
) -> Option<(usize, PreparedExpression)> {
    let PreparedExpression::Binary {
        left,
        operator: ast::BinaryOperator::Eq,
        right,
        ..
    } = expression
    else {
        return None;
    };
    match (*left, *right) {
        (
            PreparedExpression::Column {
                slot,
                data_type: BaseType::Int4,
            },
            value @ (PreparedExpression::Literal {
                data_type: BaseType::Int4,
                ..
            }
            | PreparedExpression::Parameter {
                data_type: BaseType::Int4,
                ..
            }),
        )
        | (
            value @ (PreparedExpression::Literal {
                data_type: BaseType::Int4,
                ..
            }
            | PreparedExpression::Parameter {
                data_type: BaseType::Int4,
                ..
            }),
            PreparedExpression::Column {
                slot,
                data_type: BaseType::Int4,
            },
        ) => Some((slot, value)),
        _ => None,
    }
}

pub(crate) fn build_prepared_join_plan(
    state: &DatabaseState,
    statement: &ast::Statement,
    parameter_types: &[BaseType],
    described_columns: Option<&[ColumnMeta]>,
) -> Result<Option<PreparedReadPlan>> {
    let ast::Statement::Query(query) = statement else {
        return Ok(None);
    };
    if query.with.is_some()
        || query.order_by.is_some()
        || query.limit_clause.is_some()
        || query.fetch.is_some()
        || !query.locks.is_empty()
        || query.for_clause.is_some()
        || query.settings.is_some()
        || query.format_clause.is_some()
        || !query.pipe_operators.is_empty()
    {
        return Ok(None);
    }
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        return Ok(None);
    };
    let ast::GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
        return Ok(None);
    };
    if !select.optimizer_hints.is_empty()
        || select.distinct.is_some()
        || select.select_modifiers.is_some()
        || select.top.is_some()
        || select.exclude.is_some()
        || select.into.is_some()
        || !select.lateral_views.is_empty()
        || select.prewhere.is_some()
        || !select.connect_by.is_empty()
        || !group_by.is_empty()
        || !modifiers.is_empty()
        || !select.cluster_by.is_empty()
        || !select.distribute_by.is_empty()
        || !select.sort_by.is_empty()
        || select.having.is_some()
        || !select.named_window.is_empty()
        || select.qualify.is_some()
        || select.value_table_mode.is_some()
        || select.from.len() != 1
    {
        return Ok(None);
    }
    if select.from[0].joins.len() > 1 {
        return build_prepared_unfiltered_join_chain_plan(
            state,
            statement,
            select,
            described_columns,
        );
    }
    if select.from[0].joins.len() != 1 {
        return Ok(None);
    }
    let ast::TableFactor::Table {
        name: left_name,
        args: None,
        ..
    } = &select.from[0].relation
    else {
        return Ok(None);
    };
    let join = &select.from[0].joins[0];
    let ast::TableFactor::Table {
        name: right_name,
        args: None,
        ..
    } = &join.relation
    else {
        return Ok(None);
    };
    let (condition, preserve_left) = match &join.join_operator {
        ast::JoinOperator::Join(ast::JoinConstraint::On(condition))
        | ast::JoinOperator::Inner(ast::JoinConstraint::On(condition)) => (condition, false),
        ast::JoinOperator::Left(ast::JoinConstraint::On(condition))
        | ast::JoinOperator::LeftOuter(ast::JoinConstraint::On(condition)) => (condition, true),
        _ => return Ok(None),
    };
    if preserve_left && select.selection.is_some() {
        return Ok(None);
    }
    let ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::Eq,
        right,
    } = condition
    else {
        return Ok(None);
    };
    let left_name = normalize_relation_name(left_name)?;
    let right_name = normalize_relation_name(right_name)?;
    for name in [&left_name, &right_name] {
        if super::describe_visible_system_relation(&state.catalog, name).is_some()
            || state.catalog.require_named_view(name).is_ok()
        {
            return Ok(None);
        }
    }
    let left_schema = state.catalog.require_named_table(&left_name)?;
    let right_schema = state.catalog.require_named_table(&right_name)?;
    let scope = bind_query_scope(&state.catalog, select)?;
    let left_width = left_schema.columns.len();
    let (left_key, right_key, wide_key) = match (
        try_resolve_column_reference(left, &scope),
        try_resolve_column_reference(right, &scope),
    ) {
        (Some((left_slot, left_type)), Some((right_slot, right_type)))
            if left_slot < left_width
                && right_slot >= left_width
                && left_type.base == right_type.base
                && matches!(
                    left_type.base,
                    BaseType::Int2 | BaseType::Int4 | BaseType::Int8
                ) =>
        {
            (
                left_slot,
                right_slot - left_width,
                left_type.base != BaseType::Int4,
            )
        }
        (Some((right_slot, right_type)), Some((left_slot, left_type)))
            if left_slot < left_width
                && right_slot >= left_width
                && left_type.base == right_type.base
                && matches!(
                    left_type.base,
                    BaseType::Int2 | BaseType::Int4 | BaseType::Int8
                ) =>
        {
            (
                left_slot,
                right_slot - left_width,
                left_type.base != BaseType::Int4,
            )
        }
        _ => return Ok(None),
    };
    if wide_key && !preserve_left {
        return Ok(None);
    }
    let filters = match select.selection.as_ref() {
        Some(ast::Expr::BinaryOp {
            left,
            op: ast::BinaryOperator::And,
            right,
        }) => vec![left.as_ref(), right.as_ref()],
        Some(expression) => vec![expression],
        None => Vec::new(),
    };
    let mut left_filter = None;
    let mut right_filter = None;
    for expression in filters {
        let Some(bound) = bind_prepared_expression(expression, &scope, parameter_types)? else {
            return Ok(None);
        };
        let Some((slot, value)) = extract_integer_join_filter(bound) else {
            return Ok(None);
        };
        if slot < left_width {
            if left_filter.replace((slot, value)).is_some() {
                return Ok(None);
            }
        } else if right_filter.replace((slot - left_width, value)).is_some() {
            return Ok(None);
        }
    }
    let reverse_sources = left_filter.is_none() && right_filter.is_some();
    if reverse_sources
        && !state
            .tables
            .get(&left_schema.id)
            .is_some_and(|table| table.has_unique_index(&[left_key]))
    {
        return Ok(None);
    }
    let mut projection = Vec::with_capacity(select.projection.len());
    for item in &select.projection {
        let expression = match item {
            ast::SelectItem::UnnamedExpr(expression)
            | ast::SelectItem::ExprWithAlias {
                expr: expression, ..
            } => expression,
            _ => return Ok(None),
        };
        let Some((slot, _)) = try_resolve_column_reference(expression, &scope) else {
            return Ok(None);
        };
        projection.push(if reverse_sources {
            if slot < left_width {
                right_schema.columns.len() + slot
            } else {
                slot - left_width
            }
        } else {
            slot
        });
    }
    let columns = match described_columns {
        Some(columns) => columns.to_vec(),
        None => describe_query_result_columns(state, statement)?,
    };
    if left_filter.is_none() && right_filter.is_none() {
        let plan = PreparedUnfilteredInnerJoinPlan {
            left_table_id: left_schema.id,
            right_table_id: right_schema.id,
            left_width,
            left_key,
            right_key,
            projection,
            columns,
        };
        return Ok(Some(if wide_key {
            PreparedReadPlan::UnfilteredWideLeftJoin(plan)
        } else if preserve_left {
            PreparedReadPlan::UnfilteredLeftJoin(plan)
        } else {
            PreparedReadPlan::UnfilteredInnerJoin(plan)
        }));
    }
    let (filter_slot, filter_value) = if let Some(filter) = left_filter {
        filter
    } else {
        right_filter.take().expect("right-only join has a filter")
    };
    let (left_schema, right_schema, left_width, left_key, right_key) = if reverse_sources {
        (
            right_schema,
            left_schema,
            right_schema.columns.len(),
            right_key,
            left_key,
        )
    } else {
        (left_schema, right_schema, left_width, left_key, right_key)
    };
    let right_is_unique = state
        .tables
        .get(&right_schema.id)
        .is_some_and(|table| table.has_unique_index(&[right_key]));
    let use_unique_probe = right_is_unique
        && (filter_slot == left_key
            || state
                .tables
                .get(&left_schema.id)
                .is_some_and(|table| table.has_unique_index(&[filter_slot])));
    let plan = PreparedInnerJoinPlan {
        left_table_id: left_schema.id,
        right_table_id: right_schema.id,
        left_width,
        left_key,
        right_key,
        filter_slot,
        filter_value,
        projection,
        columns,
    };
    Ok(Some(
        if let Some((right_filter_slot, right_filter_value)) = right_filter {
            PreparedReadPlan::DoubleFilteredInnerJoin(Box::new(
                PreparedDoubleFilteredInnerJoinPlan {
                    join: plan,
                    right_filter_slot,
                    right_filter_value,
                },
            ))
        } else if use_unique_probe {
            PreparedReadPlan::UniqueInnerJoin(plan)
        } else if right_is_unique {
            PreparedReadPlan::AdaptiveUniqueInnerJoin(plan)
        } else {
            PreparedReadPlan::InnerJoin(plan)
        },
    ))
}

fn build_prepared_unfiltered_join_chain_plan(
    state: &DatabaseState,
    statement: &ast::Statement,
    select: &ast::Select,
    described_columns: Option<&[ColumnMeta]>,
) -> Result<Option<PreparedReadPlan>> {
    if select.selection.is_some() {
        return Ok(None);
    }
    let table = &select.from[0];
    let mut table_ids = Vec::with_capacity(table.joins.len() + 1);
    let mut starts = Vec::with_capacity(table.joins.len() + 1);
    let mut next_slot = 0;
    for factor in
        std::iter::once(&table.relation).chain(table.joins.iter().map(|join| &join.relation))
    {
        let ast::TableFactor::Table {
            name, args: None, ..
        } = factor
        else {
            return Ok(None);
        };
        let name = normalize_relation_name(name)?;
        if super::describe_visible_system_relation(&state.catalog, &name).is_some()
            || state.catalog.require_named_view(&name).is_ok()
        {
            return Ok(None);
        }
        let schema = state.catalog.require_named_table(&name)?;
        starts.push(next_slot);
        next_slot += schema.columns.len();
        table_ids.push(schema.id);
    }
    let scope = bind_query_scope(&state.catalog, select)?;
    let mut joins = Vec::with_capacity(table.joins.len());
    for (index, join) in table.joins.iter().enumerate() {
        let condition = match &join.join_operator {
            ast::JoinOperator::Join(ast::JoinConstraint::On(condition))
            | ast::JoinOperator::Inner(ast::JoinConstraint::On(condition)) => condition,
            _ => return Ok(None),
        };
        let ast::Expr::BinaryOp {
            left,
            op: ast::BinaryOperator::Eq,
            right,
        } = condition
        else {
            return Ok(None);
        };
        let (Some((first_slot, first_type)), Some((second_slot, second_type))) = (
            try_resolve_column_reference(left, &scope),
            try_resolve_column_reference(right, &scope),
        ) else {
            return Ok(None);
        };
        if first_type.base != second_type.base
            || !matches!(
                first_type.base,
                BaseType::Int2 | BaseType::Int4 | BaseType::Int8
            )
        {
            return Ok(None);
        }
        let right_start = starts[index + 1];
        let right_end = starts.get(index + 2).copied().unwrap_or(next_slot);
        let (left_slot, right_slot) =
            if first_slot < right_start && (right_start..right_end).contains(&second_slot) {
                (first_slot, second_slot)
            } else if second_slot < right_start && (right_start..right_end).contains(&first_slot) {
                (second_slot, first_slot)
            } else {
                return Ok(None);
            };
        let left_source = starts.partition_point(|&start| start <= left_slot) - 1;
        joins.push((
            left_source,
            left_slot - starts[left_source],
            right_slot - right_start,
            first_type.base != BaseType::Int4,
        ));
    }
    let mut projection = Vec::with_capacity(select.projection.len());
    for item in &select.projection {
        let expression = match item {
            ast::SelectItem::UnnamedExpr(expression)
            | ast::SelectItem::ExprWithAlias {
                expr: expression, ..
            } => expression,
            _ => return Ok(None),
        };
        let Some((slot, _)) = try_resolve_column_reference(expression, &scope) else {
            return Ok(None);
        };
        if slot >= next_slot {
            return Ok(None);
        }
        let source = starts.partition_point(|&start| start <= slot) - 1;
        projection.push((source, slot - starts[source]));
    }
    let columns = match described_columns {
        Some(columns) => columns.to_vec(),
        None => describe_query_result_columns(state, statement)?,
    };
    let plan = PreparedUnfilteredJoinChainPlan {
        table_ids,
        joins,
        projection,
        columns,
    };
    Ok(Some(PreparedReadPlan::UnfilteredJoinChain(plan)))
}

pub(crate) fn execute_prepared_read(
    state: &DatabaseState,
    plan: &PreparedReadPlan,
    parameters: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: Option<&StatementContext>,
    deadline: Option<Instant>,
    timezone: &str,
) -> Result<Vec<Vec<Value>>> {
    match plan {
        PreparedReadPlan::Query(plan) => super::execute_prepared_query(
            state, plan, parameters, xid, snapshot, context, deadline, timezone,
        ),
        PreparedReadPlan::UniqueInnerJoin(plan) => {
            execute_prepared_unique_join(state, plan, parameters, xid, snapshot, deadline, timezone)
        }
        PreparedReadPlan::AdaptiveUniqueInnerJoin(plan) => execute_prepared_adaptive_unique_join(
            state, plan, parameters, xid, snapshot, deadline, timezone,
        ),
        PreparedReadPlan::DoubleFilteredInnerJoin(plan) => {
            let right_filter_value = evaluate_prepared_expression(
                &plan.right_filter_value,
                &[],
                parameters,
                deadline,
                timezone,
            )?;
            execute_prepared_inner_join::<true>(
                state,
                &plan.join,
                parameters,
                xid,
                snapshot,
                deadline,
                timezone,
                Some((plan.right_filter_slot, &right_filter_value)),
            )
        }
        PreparedReadPlan::InnerJoin(plan) => execute_prepared_inner_join::<false>(
            state, plan, parameters, xid, snapshot, deadline, timezone, None,
        ),
        PreparedReadPlan::UnfilteredInnerJoin(plan) => {
            execute_prepared_unfiltered_join::<false>(state, plan, xid, snapshot, deadline)
        }
        PreparedReadPlan::UnfilteredLeftJoin(plan) => {
            execute_prepared_unfiltered_join::<true>(state, plan, xid, snapshot, deadline)
        }
        PreparedReadPlan::UnfilteredWideLeftJoin(plan) => {
            execute_prepared_wide_left_join(state, plan, xid, snapshot, deadline)
        }
        PreparedReadPlan::UnfilteredJoinChain(plan) => {
            if plan.joins.iter().any(|join| join.3) {
                execute_prepared_unfiltered_join_chain(
                    state,
                    plan,
                    xid,
                    snapshot,
                    deadline,
                    |value| match value {
                        Value::Int2(key) => Some(i64::from(*key)),
                        Value::Int4(key) => Some(i64::from(*key)),
                        Value::Int8(key) => Some(*key),
                        _ => None,
                    },
                )
            } else {
                execute_prepared_unfiltered_join_chain(
                    state,
                    plan,
                    xid,
                    snapshot,
                    deadline,
                    |value| match value {
                        Value::Int4(key) => Some(*key),
                        _ => None,
                    },
                )
            }
        }
    }
}

#[inline(never)]
fn execute_prepared_unfiltered_join<const PRESERVE_LEFT: bool>(
    state: &DatabaseState,
    plan: &PreparedUnfilteredInnerJoinPlan,
    xid: Xid,
    snapshot: &Snapshot,
    deadline: Option<Instant>,
) -> Result<Vec<Vec<Value>>> {
    state.catalog.require_table_by_id(plan.left_table_id)?;
    state.catalog.require_table_by_id(plan.right_table_id)?;
    let left_table = state
        .tables
        .get(&plan.left_table_id)
        .expect("prepared left table must have storage");
    let right_table = state
        .tables
        .get(&plan.right_table_id)
        .expect("prepared right table must have storage");
    let mut left_rows = Vec::new();
    state.record_read(xid, Access::Relation(plan.left_table_id));
    for (row_id, chain) in left_table.iterate_version_chains() {
        let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions) else {
            continue;
        };
        state.record_read(xid, Access::Row(plan.left_table_id, row_id));
        left_rows.push(&version.row);
    }
    let mut right_by_key = HashMap::<i32, Vec<&[Value]>>::new();
    state.record_read(xid, Access::Relation(plan.right_table_id));
    for (row_id, chain) in right_table.iterate_version_chains() {
        let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions) else {
            continue;
        };
        state.record_read(xid, Access::Row(plan.right_table_id, row_id));
        if let Value::Int4(key) = version.row[plan.right_key] {
            right_by_key.entry(key).or_default().push(&version.row);
        }
    }
    let mut rows = Vec::new();
    for left in left_rows {
        let matches = match left[plan.left_key] {
            Value::Int4(key) => right_by_key.get(&key),
            _ => None,
        };
        if let Some(matches) = matches {
            for right in matches {
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Err(PgError::create(
                        SqlState::QueryCanceled,
                        "canceling statement due to statement timeout",
                    ));
                }
                rows.push(
                    plan.projection
                        .iter()
                        .map(|slot| {
                            if *slot < plan.left_width {
                                left[*slot].clone()
                            } else {
                                right[*slot - plan.left_width].clone()
                            }
                        })
                        .collect(),
                );
            }
        } else if PRESERVE_LEFT {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(PgError::create(
                    SqlState::QueryCanceled,
                    "canceling statement due to statement timeout",
                ));
            }
            rows.push(
                plan.projection
                    .iter()
                    .map(|slot| {
                        if *slot < plan.left_width {
                            left[*slot].clone()
                        } else {
                            Value::Null
                        }
                    })
                    .collect(),
            );
        }
    }
    Ok(rows)
}

#[inline(never)]
fn execute_prepared_wide_left_join(
    state: &DatabaseState,
    plan: &PreparedUnfilteredInnerJoinPlan,
    xid: Xid,
    snapshot: &Snapshot,
    deadline: Option<Instant>,
) -> Result<Vec<Vec<Value>>> {
    state.catalog.require_table_by_id(plan.left_table_id)?;
    state.catalog.require_table_by_id(plan.right_table_id)?;
    let left_table = state
        .tables
        .get(&plan.left_table_id)
        .expect("prepared left table must have storage");
    let right_table = state
        .tables
        .get(&plan.right_table_id)
        .expect("prepared right table must have storage");
    let mut left_rows = Vec::new();
    state.record_read(xid, Access::Relation(plan.left_table_id));
    for (row_id, chain) in left_table.iterate_version_chains() {
        let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions) else {
            continue;
        };
        state.record_read(xid, Access::Row(plan.left_table_id, row_id));
        left_rows.push(&version.row);
    }
    let mut right_by_key = HashMap::<i64, Vec<&[Value]>>::new();
    state.record_read(xid, Access::Relation(plan.right_table_id));
    for (row_id, chain) in right_table.iterate_version_chains() {
        let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions) else {
            continue;
        };
        state.record_read(xid, Access::Row(plan.right_table_id, row_id));
        let key = match version.row[plan.right_key] {
            Value::Int2(key) => Some(i64::from(key)),
            Value::Int8(key) => Some(key),
            _ => None,
        };
        if let Some(key) = key {
            right_by_key.entry(key).or_default().push(&version.row);
        }
    }
    let mut rows = Vec::new();
    for left in left_rows {
        let key = match left[plan.left_key] {
            Value::Int2(key) => Some(i64::from(key)),
            Value::Int8(key) => Some(key),
            _ => None,
        };
        let matches = key.and_then(|key| right_by_key.get(&key));
        if let Some(matches) = matches {
            for right in matches {
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Err(PgError::create(
                        SqlState::QueryCanceled,
                        "canceling statement due to statement timeout",
                    ));
                }
                rows.push(
                    plan.projection
                        .iter()
                        .map(|slot| {
                            if *slot < plan.left_width {
                                left[*slot].clone()
                            } else {
                                right[*slot - plan.left_width].clone()
                            }
                        })
                        .collect(),
                );
            }
        } else {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(PgError::create(
                    SqlState::QueryCanceled,
                    "canceling statement due to statement timeout",
                ));
            }
            rows.push(
                plan.projection
                    .iter()
                    .map(|slot| {
                        if *slot < plan.left_width {
                            left[*slot].clone()
                        } else {
                            Value::Null
                        }
                    })
                    .collect(),
            );
        }
    }
    Ok(rows)
}

#[inline(never)]
fn execute_prepared_unfiltered_join_chain<K: Copy + Eq + Hash>(
    state: &DatabaseState,
    plan: &PreparedUnfilteredJoinChainPlan,
    xid: Xid,
    snapshot: &Snapshot,
    deadline: Option<Instant>,
    extract_key: impl Fn(&Value) -> Option<K>,
) -> Result<Vec<Vec<Value>>> {
    for table_id in &plan.table_ids {
        state.catalog.require_table_by_id(*table_id)?;
    }
    let first_id = plan.table_ids[0];
    let first_table = state
        .tables
        .get(&first_id)
        .expect("prepared first table must have storage");
    let mut left_rows = Vec::new();
    state.record_read(xid, Access::Relation(first_id));
    for (row_id, chain) in first_table.iterate_version_chains() {
        let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions) else {
            continue;
        };
        state.record_read(xid, Access::Row(first_id, row_id));
        left_rows.push(version.row.as_slice());
    }
    for (index, &(left_source, left_key, right_key, _)) in plan.joins.iter().enumerate() {
        let right_id = plan.table_ids[index + 1];
        let right_table = state
            .tables
            .get(&right_id)
            .expect("prepared right table must have storage");
        let mut right_by_key = HashMap::<K, Vec<&[Value]>>::new();
        state.record_read(xid, Access::Relation(right_id));
        for (row_id, chain) in right_table.iterate_version_chains() {
            let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions)
            else {
                continue;
            };
            state.record_read(xid, Access::Row(right_id, row_id));
            if let Some(key) = extract_key(&version.row[right_key]) {
                right_by_key
                    .entry(key)
                    .or_default()
                    .push(version.row.as_slice());
            }
        }
        if index + 1 == plan.joins.len() {
            let mut rows = Vec::new();
            for left in left_rows.chunks_exact(index + 1) {
                let Some(key) = extract_key(&left[left_source][left_key]) else {
                    continue;
                };
                let Some(matches) = right_by_key.get(&key) else {
                    continue;
                };
                for &right in matches {
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Err(PgError::create(
                            SqlState::QueryCanceled,
                            "canceling statement due to statement timeout",
                        ));
                    }
                    rows.push(
                        plan.projection
                            .iter()
                            .map(|&(source, slot)| {
                                if source == index + 1 {
                                    right[slot].clone()
                                } else {
                                    left[source][slot].clone()
                                }
                            })
                            .collect(),
                    );
                }
            }
            return Ok(rows);
        }
        let mut joined = Vec::new();
        for left in left_rows.chunks_exact(index + 1) {
            let Some(key) = extract_key(&left[left_source][left_key]) else {
                continue;
            };
            if let Some(matches) = right_by_key.get(&key) {
                for &right in matches {
                    joined.extend_from_slice(left);
                    joined.push(right);
                }
            }
        }
        left_rows = joined;
    }
    unreachable!("prepared join chain has at least two joins")
}

fn execute_prepared_inner_join<const RIGHT_FILTER: bool>(
    state: &DatabaseState,
    plan: &PreparedInnerJoinPlan,
    parameters: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    deadline: Option<Instant>,
    timezone: &str,
    right_filter: Option<(usize, &Value)>,
) -> Result<Vec<Vec<Value>>> {
    state.catalog.require_table_by_id(plan.left_table_id)?;
    state.catalog.require_table_by_id(plan.right_table_id)?;
    let left_table = state
        .tables
        .get(&plan.left_table_id)
        .expect("prepared left table must have storage");
    let right_table = state
        .tables
        .get(&plan.right_table_id)
        .expect("prepared right table must have storage");
    let filter_value =
        evaluate_prepared_expression(&plan.filter_value, &[], parameters, deadline, timezone)?;
    let right_filter_key = if plan.filter_slot == plan.left_key {
        match &filter_value {
            Value::Int4(key) => Some(*key),
            _ => None,
        }
    } else {
        None
    };
    let mut right_by_key = HashMap::<i32, Vec<&[Value]>>::new();
    state.record_read(xid, Access::Relation(plan.right_table_id));
    for (row_id, chain) in right_table.iterate_version_chains() {
        let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions) else {
            continue;
        };
        state.record_read(xid, Access::Row(plan.right_table_id, row_id));
        if RIGHT_FILTER {
            let (slot, value) = right_filter.expect("double-filtered join has a right filter");
            if value.is_null() || &version.row[slot] != value {
                continue;
            }
        }
        if let Value::Int4(key) = version.row[plan.right_key]
            && right_filter_key.is_none_or(|filter_key| filter_key == key)
        {
            right_by_key.entry(key).or_default().push(&version.row);
        }
    }
    let mut rows = Vec::new();
    state.record_read(xid, Access::Relation(plan.left_table_id));
    for (row_id, chain) in left_table.iterate_version_chains() {
        let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions) else {
            continue;
        };
        state.record_read(xid, Access::Row(plan.left_table_id, row_id));
        if filter_value.is_null() || version.row[plan.filter_slot] != filter_value {
            continue;
        }
        let Value::Int4(key) = version.row[plan.left_key] else {
            continue;
        };
        let Some(matches) = right_by_key.get(&key) else {
            continue;
        };
        for right in matches {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(PgError::create(
                    SqlState::QueryCanceled,
                    "canceling statement due to statement timeout",
                ));
            }
            rows.push(
                plan.projection
                    .iter()
                    .map(|slot| {
                        if *slot < plan.left_width {
                            version.row[*slot].clone()
                        } else {
                            right[*slot - plan.left_width].clone()
                        }
                    })
                    .collect(),
            );
        }
    }
    Ok(rows)
}

fn execute_prepared_unique_join(
    state: &DatabaseState,
    plan: &PreparedInnerJoinPlan,
    parameters: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    deadline: Option<Instant>,
    timezone: &str,
) -> Result<Vec<Vec<Value>>> {
    state.catalog.require_table_by_id(plan.left_table_id)?;
    state.catalog.require_table_by_id(plan.right_table_id)?;
    let left_table = state
        .tables
        .get(&plan.left_table_id)
        .expect("prepared left table must have storage");
    let right_table = state
        .tables
        .get(&plan.right_table_id)
        .expect("prepared right table must have storage");
    let left_unique = left_table.has_unique_index(&[plan.filter_slot]);
    if !right_table.has_unique_index(&[plan.right_key])
        || (!left_unique && plan.filter_slot != plan.left_key)
    {
        return execute_prepared_inner_join::<false>(
            state, plan, parameters, xid, snapshot, deadline, timezone, None,
        );
    }
    let filter_value =
        evaluate_prepared_expression(&plan.filter_value, &[], parameters, deadline, timezone)?;
    if filter_value.is_null() {
        return Ok(Vec::new());
    }
    if !left_unique {
        let mut left_rows = Vec::new();
        state.record_read(xid, Access::Relation(plan.left_table_id));
        for (row_id, chain) in left_table.iterate_version_chains() {
            let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions)
            else {
                continue;
            };
            state.record_read(xid, Access::Row(plan.left_table_id, row_id));
            if version.row[plan.filter_slot] == filter_value {
                left_rows.push(&version.row);
            }
        }
        if left_rows.is_empty() {
            return Ok(Vec::new());
        }
        if state.tracks_serializable_reads(xid)
            && let Some(key) = right_table
                .create_unique_read_key(&[plan.right_key], std::slice::from_ref(&filter_value))
        {
            state.record_read(
                xid,
                Access::Unique(plan.right_table_id, vec![plan.right_key], key),
            );
        }
        let Some((right_row_id, right_version)) = right_table.find_unique_visible_version(
            &[plan.right_key],
            std::slice::from_ref(&filter_value),
            snapshot,
            xid,
            &state.transactions,
        ) else {
            return Ok(Vec::new());
        };
        state.record_read(xid, Access::Row(plan.right_table_id, right_row_id));
        let mut rows = Vec::with_capacity(left_rows.len());
        for left in left_rows {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(PgError::create(
                    SqlState::QueryCanceled,
                    "canceling statement due to statement timeout",
                ));
            }
            rows.push(
                plan.projection
                    .iter()
                    .map(|slot| {
                        if *slot < plan.left_width {
                            left[*slot].clone()
                        } else {
                            right_version.row[*slot - plan.left_width].clone()
                        }
                    })
                    .collect(),
            );
        }
        return Ok(rows);
    }
    if state.tracks_serializable_reads(xid)
        && let Some(key) = left_table
            .create_unique_read_key(&[plan.filter_slot], std::slice::from_ref(&filter_value))
    {
        state.record_read(
            xid,
            Access::Unique(plan.left_table_id, vec![plan.filter_slot], key),
        );
    }
    let Some((left_row_id, left_version)) = left_table.find_unique_visible_version(
        &[plan.filter_slot],
        std::slice::from_ref(&filter_value),
        snapshot,
        xid,
        &state.transactions,
    ) else {
        return Ok(Vec::new());
    };
    state.record_read(xid, Access::Row(plan.left_table_id, left_row_id));
    let join_key = &left_version.row[plan.left_key];
    if join_key.is_null() {
        return Ok(Vec::new());
    }
    if state.tracks_serializable_reads(xid)
        && let Some(key) =
            right_table.create_unique_read_key(&[plan.right_key], std::slice::from_ref(join_key))
    {
        state.record_read(
            xid,
            Access::Unique(plan.right_table_id, vec![plan.right_key], key),
        );
    }
    let Some((right_row_id, right_version)) = right_table.find_unique_visible_version(
        &[plan.right_key],
        std::slice::from_ref(join_key),
        snapshot,
        xid,
        &state.transactions,
    ) else {
        return Ok(Vec::new());
    };
    state.record_read(xid, Access::Row(plan.right_table_id, right_row_id));
    Ok(vec![
        plan.projection
            .iter()
            .map(|slot| {
                if *slot < plan.left_width {
                    left_version.row[*slot].clone()
                } else {
                    right_version.row[*slot - plan.left_width].clone()
                }
            })
            .collect(),
    ])
}

#[inline(never)]
fn execute_prepared_adaptive_unique_join(
    state: &DatabaseState,
    plan: &PreparedInnerJoinPlan,
    parameters: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    deadline: Option<Instant>,
    timezone: &str,
) -> Result<Vec<Vec<Value>>> {
    state.catalog.require_table_by_id(plan.left_table_id)?;
    state.catalog.require_table_by_id(plan.right_table_id)?;
    let left_table = state
        .tables
        .get(&plan.left_table_id)
        .expect("prepared left table must have storage");
    let right_table = state
        .tables
        .get(&plan.right_table_id)
        .expect("prepared right table must have storage");
    if !right_table.has_unique_index(&[plan.right_key]) {
        return execute_prepared_inner_join::<false>(
            state, plan, parameters, xid, snapshot, deadline, timezone, None,
        );
    }
    if plan.filter_slot == plan.left_key || left_table.has_unique_index(&[plan.filter_slot]) {
        return execute_prepared_unique_join(
            state, plan, parameters, xid, snapshot, deadline, timezone,
        );
    }
    let filter_value =
        evaluate_prepared_expression(&plan.filter_value, &[], parameters, deadline, timezone)?;
    if filter_value.is_null() {
        return Ok(Vec::new());
    }
    execute_prepared_off_key_unique_join(
        state,
        plan,
        left_table,
        right_table,
        &filter_value,
        xid,
        snapshot,
        deadline,
    )
}

#[inline(never)]
fn execute_prepared_off_key_unique_join(
    state: &DatabaseState,
    plan: &PreparedInnerJoinPlan,
    left_table: &crate::storage::Table,
    right_table: &crate::storage::Table,
    filter_value: &Value,
    xid: Xid,
    snapshot: &Snapshot,
    deadline: Option<Instant>,
) -> Result<Vec<Vec<Value>>> {
    let max_probes = right_table.iterate_version_chains().size_hint().0 / 32;
    let mut left_iter = left_table.iterate_version_chains();
    let mut left_rows = Vec::new();
    state.record_read(xid, Access::Relation(plan.left_table_id));
    while left_rows.len() <= max_probes {
        let Some((row_id, chain)) = left_iter.next() else {
            break;
        };
        let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions) else {
            continue;
        };
        state.record_read(xid, Access::Row(plan.left_table_id, row_id));
        if &version.row[plan.filter_slot] == filter_value {
            left_rows.push(&version.row);
        }
    }
    let mut rows = Vec::new();
    if left_rows.len() <= max_probes {
        for left in left_rows {
            let join_key = &left[plan.left_key];
            if join_key.is_null() {
                continue;
            }
            if state.tracks_serializable_reads(xid)
                && let Some(key) = right_table
                    .create_unique_read_key(&[plan.right_key], std::slice::from_ref(join_key))
            {
                state.record_read(
                    xid,
                    Access::Unique(plan.right_table_id, vec![plan.right_key], key),
                );
            }
            let Some((right_row_id, right_version)) = right_table.find_unique_visible_version(
                &[plan.right_key],
                std::slice::from_ref(join_key),
                snapshot,
                xid,
                &state.transactions,
            ) else {
                continue;
            };
            state.record_read(xid, Access::Row(plan.right_table_id, right_row_id));
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(PgError::create(
                    SqlState::QueryCanceled,
                    "canceling statement due to statement timeout",
                ));
            }
            rows.push(
                plan.projection
                    .iter()
                    .map(|slot| {
                        if *slot < plan.left_width {
                            left[*slot].clone()
                        } else {
                            right_version.row[*slot - plan.left_width].clone()
                        }
                    })
                    .collect(),
            );
        }
    } else {
        let mut right_by_key = HashMap::<i32, Vec<&[Value]>>::new();
        state.record_read(xid, Access::Relation(plan.right_table_id));
        for (row_id, chain) in right_table.iterate_version_chains() {
            let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions)
            else {
                continue;
            };
            state.record_read(xid, Access::Row(plan.right_table_id, row_id));
            if let Value::Int4(key) = version.row[plan.right_key] {
                right_by_key.entry(key).or_default().push(&version.row);
            }
        }
        let remaining_left = left_iter.filter_map(|(row_id, chain)| {
            let version = find_visible_version(chain, snapshot, xid, &state.transactions)?;
            state.record_read(xid, Access::Row(plan.left_table_id, row_id));
            (&version.row[plan.filter_slot] == filter_value).then_some(&version.row)
        });
        for left in left_rows.into_iter().chain(remaining_left) {
            let Value::Int4(key) = left[plan.left_key] else {
                continue;
            };
            let Some(matches) = right_by_key.get(&key) else {
                continue;
            };
            for right in matches {
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Err(PgError::create(
                        SqlState::QueryCanceled,
                        "canceling statement due to statement timeout",
                    ));
                }
                rows.push(
                    plan.projection
                        .iter()
                        .map(|slot| {
                            if *slot < plan.left_width {
                                left[*slot].clone()
                            } else {
                                right[*slot - plan.left_width].clone()
                            }
                        })
                        .collect(),
                );
            }
        }
    }
    Ok(rows)
}
