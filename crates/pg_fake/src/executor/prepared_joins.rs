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
    UniqueInnerJoin(PreparedInnerJoinPlan),
    DoubleFilteredInnerJoin(Box<PreparedDoubleFilteredInnerJoinPlan>),
}

impl PreparedReadPlan {
    pub(crate) fn columns(&self) -> &[ColumnMeta] {
        match self {
            Self::Query(plan) => plan.columns(),
            Self::InnerJoin(plan) | Self::UniqueInnerJoin(plan) => &plan.columns,
            Self::DoubleFilteredInnerJoin(plan) => &plan.join.columns,
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
    let filters = match selection {
        ast::Expr::BinaryOp {
            left,
            op: ast::BinaryOperator::And,
            right,
        } => vec![left.as_ref(), right.as_ref()],
        expression => vec![expression],
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
    let Some((filter_slot, filter_value)) = left_filter else {
        return Ok(None);
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
    let use_unique_probe = state
        .tables
        .get(&left_schema.id)
        .is_some_and(|table| table.has_unique_index(&[filter_slot]))
        && state
            .tables
            .get(&right_schema.id)
            .is_some_and(|table| table.has_unique_index(&[right_key]));
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
        } else {
            PreparedReadPlan::InnerJoin(plan)
        },
    ))
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
    }
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
    if !left_table.has_unique_index(&[plan.filter_slot])
        || !right_table.has_unique_index(&[plan.right_key])
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
    if let Some(key) =
        left_table.create_unique_read_key(&[plan.filter_slot], std::slice::from_ref(&filter_value))
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
    if let Some(key) =
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
