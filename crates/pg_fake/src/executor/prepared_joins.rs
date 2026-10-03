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
use std::{collections::HashMap, time::Instant};

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
}

impl PreparedReadPlan {
    pub(crate) fn columns(&self) -> &[ColumnMeta] {
        match self {
            Self::Query(plan) => plan.columns(),
            Self::InnerJoin(plan) => &plan.columns,
        }
    }
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

pub(crate) fn build_prepared_join_plan(
    state: &DatabaseState,
    statement: &ast::Statement,
    parameter_types: &[BaseType],
    described_columns: Option<&[ColumnMeta]>,
) -> Result<Option<PreparedInnerJoinPlan>> {
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
        || select.from[0].joins.len() != 1
    {
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
    let (left_key, right_key) = match (
        try_resolve_column_reference(left, &scope),
        try_resolve_column_reference(right, &scope),
    ) {
        (Some((left_slot, left_type)), Some((right_slot, right_type)))
            if left_slot < left_width
                && right_slot >= left_width
                && left_type.base == BaseType::Int4
                && right_type.base == BaseType::Int4 =>
        {
            (left_slot, right_slot - left_width)
        }
        (Some((right_slot, right_type)), Some((left_slot, left_type)))
            if left_slot < left_width
                && right_slot >= left_width
                && left_type.base == BaseType::Int4
                && right_type.base == BaseType::Int4 =>
        {
            (left_slot, right_slot - left_width)
        }
        _ => return Ok(None),
    };
    let Some(selection) = select.selection.as_ref() else {
        return Ok(None);
    };
    let Some(PreparedExpression::Binary {
        left,
        operator: ast::BinaryOperator::Eq,
        right,
        ..
    }) = bind_prepared_expression(selection, &scope, parameter_types)?
    else {
        return Ok(None);
    };
    let (filter_slot, filter_value) = match (*left, *right) {
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
        ) if slot < left_width => (slot, value),
        (
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
        ) if slot < left_width => (slot, value),
        _ => return Ok(None),
    };
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
        projection.push(slot);
    }
    let columns = match described_columns {
        Some(columns) => columns.to_vec(),
        None => describe_query_result_columns(state, statement)?,
    };
    Ok(Some(PreparedInnerJoinPlan {
        left_table_id: left_schema.id,
        right_table_id: right_schema.id,
        left_width,
        left_key,
        right_key,
        filter_slot,
        filter_value,
        projection,
        columns,
    }))
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
        PreparedReadPlan::InnerJoin(plan) => {
            execute_prepared_inner_join(state, plan, parameters, xid, snapshot, deadline, timezone)
        }
    }
}

fn execute_prepared_inner_join(
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
    let filter_value =
        evaluate_prepared_expression(&plan.filter_value, &[], parameters, deadline, timezone)?;
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
