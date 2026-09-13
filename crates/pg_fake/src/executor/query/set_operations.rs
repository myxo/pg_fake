use std::cmp::Ordering;

use sqlparser::ast;

use crate::{
    ColumnMeta, QueryResult,
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{
        DatabaseState, StatementContext,
        equality::are_rows_not_distinct,
        expressions::{
            compare_values, extract_number_literal, extract_unknown_string_literal,
            validate_equality_type, validate_ordering_type,
        },
        normalize_identifier, resolve_order_ascending,
        scope::identify_unknown_set_operand_columns,
    },
    txn::{Snapshot, Xid},
    value::{BaseType, PgType, Value},
};

use super::{describe_query_result_columns, execute_query, resolve_select_limit};

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_set_query(
    state: &DatabaseState,
    query: &ast::Query,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    maximum_rows: Option<usize>,
) -> Result<super::QueryOutput> {
    let (query_limit, offset) = resolve_select_limit(query, context)?;
    let limit = match (query_limit, maximum_rows) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    if query.order_by.is_none()
        && context.capture_lock_queries
        && let ast::SetExpr::SetOperation {
            op: ast::SetOperator::Union,
            set_quantifier: ast::SetQuantifier::All,
            left,
            right,
        } = query.body.as_ref()
    {
        let metadata = build_set_expression_metadata(state, query, &query.body)?;
        let (left_metadata, right_metadata) = metadata
            .operands
            .as_ref()
            .expect("set operation has two operands");
        validate_unknown_set_operand_columns(left, &left_metadata.unknown, &metadata.columns)?;
        validate_unknown_set_operand_columns(right, &right_metadata.unknown, &metadata.columns)?;
        let mut rows = Vec::new();
        let mut complete = true;
        if limit != Some(0) {
            let demand = limit.map(|limit| offset.saturating_add(limit));
            for operand in [left, right] {
                if demand.is_some_and(|demand| rows.len() >= demand) {
                    complete = false;
                    break;
                }
                let mut invocation = context.clone();
                invocation.query_row_demand = demand.map(|demand| demand - rows.len());
                let output = execute_query(
                    state,
                    &create_set_operand_query(query, operand),
                    xid,
                    snapshot,
                    &invocation,
                )?;
                rows.extend(coerce_set_rows(
                    output.result.rows,
                    &output.result.columns,
                    &metadata.columns,
                )?);
                if !output.complete {
                    complete = false;
                    break;
                }
            }
        }
        let mut output = super::QueryOutput::create(QueryResult {
            columns: metadata.columns,
            rows: rows
                .into_iter()
                .skip(offset)
                .take(limit.unwrap_or(usize::MAX))
                .collect(),
        });
        output.complete =
            complete || query_limit.is_some_and(|limit| output.result.rows.len() >= limit);
        return Ok(output);
    }
    let mut result =
        execute_set_expression(state, query, &query.body, None, xid, snapshot, context)?;
    sort_set_rows(&mut result.rows, &result.columns, query)?;
    let complete = limit.is_none_or(|limit| result.rows.len() <= offset.saturating_add(limit))
        || query_limit == limit;
    result.rows = result
        .rows
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    let mut output = super::QueryOutput::create(result);
    output.complete = complete;
    Ok(output)
}

struct SetExpressionMetadata {
    columns: Vec<ColumnMeta>,
    unknown: Vec<bool>,
    operands: Option<(Box<SetExpressionMetadata>, Box<SetExpressionMetadata>)>,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn describe_set_expression_columns(
    state: &DatabaseState,
    query: &ast::Query,
    expression: &ast::SetExpr,
) -> Result<Vec<ColumnMeta>> {
    match expression {
        ast::SetExpr::Query(query) => {
            describe_query_result_columns(state, &ast::Statement::Query(query.clone()))
        }
        ast::SetExpr::SetOperation { left, right, .. } => {
            let left_columns = describe_set_expression_columns(state, query, left)?;
            let right_columns = describe_set_expression_columns(state, query, right)?;
            resolve_set_columns_with_unknown(
                &left_columns,
                &right_columns,
                &identify_unknown_set_operand_columns(left, left_columns.len()),
                &identify_unknown_set_operand_columns(right, right_columns.len()),
            )
        }
        ast::SetExpr::Select(_) | ast::SetExpr::Values(_) => {
            let operand = create_set_operand_query(query, expression);
            describe_query_result_columns(state, &ast::Statement::Query(Box::new(operand)))
        }
        _ => reject_unsupported("set-operation input is not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn create_set_operand_query(
    query: &ast::Query,
    expression: &ast::SetExpr,
) -> ast::Query {
    ast::Query {
        with: None,
        body: Box::new(expression.clone()),
        order_by: None,
        limit_clause: None,
        fetch: None,
        locks: Vec::new(),
        for_clause: query.for_clause.clone(),
        settings: query.settings.clone(),
        format_clause: query.format_clause.clone(),
        pipe_operators: query.pipe_operators.clone(),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validate_unknown_set_operand_columns(
    expression: &ast::SetExpr,
    unknown: &[bool],
    columns: &[ColumnMeta],
) -> Result<()> {
    let select = match expression {
        ast::SetExpr::Select(select) => select.as_ref(),
        ast::SetExpr::Query(query) => match query.body.as_ref() {
            ast::SetExpr::Select(select) => select.as_ref(),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };
    assert_eq!(unknown.len(), columns.len());
    for ((item, unknown), column) in select.projection.iter().zip(unknown).zip(columns) {
        if !unknown {
            continue;
        }
        let expression = match item {
            ast::SelectItem::UnnamedExpr(expression)
            | ast::SelectItem::ExprWithAlias {
                expr: expression, ..
            } => expression,
            _ => continue,
        };
        if let Some(text) = extract_unknown_string_literal(expression) {
            coercion::coerce_unknown(
                text,
                PgType::create(
                    BaseType::resolve_oid(column.type_oid)
                        .expect("set-operation column has a supported type OID"),
                ),
                CastContext::Implicit,
                "UTC",
            )?;
        }
    }
    Ok(())
}

fn build_set_expression_metadata(
    state: &DatabaseState,
    query: &ast::Query,
    expression: &ast::SetExpr,
) -> Result<SetExpressionMetadata> {
    let (columns, operands) = if let ast::SetExpr::SetOperation { left, right, .. } = expression {
        let left = build_set_expression_metadata(state, query, left)?;
        let right = build_set_expression_metadata(state, query, right)?;
        let columns = resolve_set_columns_with_unknown(
            &left.columns,
            &right.columns,
            &left.unknown,
            &right.unknown,
        )?;
        (columns, Some((Box::new(left), Box::new(right))))
    } else {
        (
            describe_set_expression_columns(state, query, expression)?,
            None,
        )
    };
    let unknown = identify_unknown_set_operand_columns(expression, columns.len());
    Ok(SetExpressionMetadata {
        columns,
        unknown,
        operands,
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_set_expression(
    state: &DatabaseState,
    query: &ast::Query,
    expression: &ast::SetExpr,
    metadata: Option<&SetExpressionMetadata>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<QueryResult> {
    match expression {
        ast::SetExpr::Query(query) => {
            let result = execute_query(state, query, xid, snapshot, context)?.result;
            Ok(result)
        }
        ast::SetExpr::SetOperation {
            op,
            set_quantifier,
            left,
            right,
        } => {
            let planned;
            let metadata = match metadata {
                Some(metadata) => metadata,
                None => {
                    planned = build_set_expression_metadata(state, query, expression)?;
                    &planned
                }
            };
            let (left_metadata, right_metadata) = metadata
                .operands
                .as_ref()
                .expect("set operation metadata contains both operands");
            validate_unknown_set_operand_columns(left, &left_metadata.unknown, &metadata.columns)?;
            validate_unknown_set_operand_columns(
                right,
                &right_metadata.unknown,
                &metadata.columns,
            )?;
            validate_set_operation_types(*op, *set_quantifier, &metadata.columns)?;
            let left = execute_set_expression(
                state,
                query,
                left,
                Some(left_metadata),
                xid,
                snapshot,
                context,
            )?;
            let right = execute_set_expression(
                state,
                query,
                right,
                Some(right_metadata),
                xid,
                snapshot,
                context,
            )?;
            execute_set_operation(
                *op,
                *set_quantifier,
                left,
                right,
                &left_metadata.unknown,
                &right_metadata.unknown,
            )
        }
        ast::SetExpr::Select(_) | ast::SetExpr::Values(_) => {
            let operand = create_set_operand_query(query, expression);
            let result = execute_query(state, &operand, xid, snapshot, context)?.result;
            Ok(result)
        }
        _ => reject_unsupported("set-operation input is not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_set_operation(
    operator: ast::SetOperator,
    quantifier: ast::SetQuantifier,
    left: QueryResult,
    right: QueryResult,
    left_unknown: &[bool],
    right_unknown: &[bool],
) -> Result<QueryResult> {
    if left.columns.len() != right.columns.len() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "each set-operation query must have the same number of columns",
        ));
    }
    let columns = resolve_set_columns_with_unknown(
        &left.columns,
        &right.columns,
        left_unknown,
        right_unknown,
    )?;
    validate_set_operation_types(operator, quantifier, &columns)?;
    let left =
        coerce_set_rows_with_unknown(left.rows, &left.columns, &columns, Some(left_unknown))?;
    let right =
        coerce_set_rows_with_unknown(right.rows, &right.columns, &columns, Some(right_unknown))?;
    let rows = match (operator, quantifier) {
        (ast::SetOperator::Union, ast::SetQuantifier::All) => {
            left.into_iter().chain(right).collect()
        }
        (ast::SetOperator::Union, ast::SetQuantifier::None | ast::SetQuantifier::Distinct) => {
            remove_set_duplicates(left.into_iter().chain(right).collect())?
        }
        (ast::SetOperator::Intersect, ast::SetQuantifier::All) => {
            select_set_intersection(left, right)?
        }
        (ast::SetOperator::Intersect, ast::SetQuantifier::None | ast::SetQuantifier::Distinct) => {
            select_set_intersection(remove_set_duplicates(left)?, remove_set_duplicates(right)?)?
        }
        (ast::SetOperator::Except, ast::SetQuantifier::All) => select_set_difference(left, right)?,
        (ast::SetOperator::Except, ast::SetQuantifier::None | ast::SetQuantifier::Distinct) => {
            select_set_difference(remove_set_duplicates(left)?, remove_set_duplicates(right)?)?
        }
        _ => return reject_unsupported("set-operation quantifier is not implemented"),
    };
    Ok(QueryResult { columns, rows })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validate_set_operation_types(
    operator: ast::SetOperator,
    quantifier: ast::SetQuantifier,
    columns: &[ColumnMeta],
) -> Result<()> {
    if !matches!(
        (operator, quantifier),
        (ast::SetOperator::Union, ast::SetQuantifier::All)
    ) {
        for column in columns {
            validate_equality_type(
                BaseType::resolve_oid(column.type_oid)
                    .expect("set-operation columns use supported PostgreSQL types"),
            )?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn resolve_set_columns(
    left: &[ColumnMeta],
    right: &[ColumnMeta],
) -> Result<Vec<ColumnMeta>> {
    resolve_set_columns_with_unknown(
        left,
        right,
        &vec![false; left.len()],
        &vec![false; right.len()],
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_set_columns_with_unknown(
    left: &[ColumnMeta],
    right: &[ColumnMeta],
    left_unknown: &[bool],
    right_unknown: &[bool],
) -> Result<Vec<ColumnMeta>> {
    if left.len() != right.len() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "each set-operation query must have the same number of columns",
        ));
    }
    assert_eq!(left.len(), left_unknown.len());
    assert_eq!(right.len(), right_unknown.len());
    left.iter()
        .zip(right)
        .zip(left_unknown.iter().zip(right_unknown))
        .map(|((left, right), (left_unknown, right_unknown))| {
            let left_type = BaseType::resolve_oid(left.type_oid)
                .expect("set-operation column has a supported type OID");
            let right_type = BaseType::resolve_oid(right.type_oid)
                .expect("set-operation column has a supported type OID");
            let data_type = match (*left_unknown, *right_unknown) {
                (true, false) => right_type,
                (false, true) => left_type,
                _ => coercion::resolve_common_type(left_type, right_type).ok_or_else(|| {
                    PgError::create(
                        SqlState::DatatypeMismatch,
                        "set-operation types cannot be matched",
                    )
                })?,
            };
            Ok(ColumnMeta {
                name: left.name.clone(),
                type_oid: data_type.map_to_oid(),
                typmod: PgType::NO_TYPEMOD,
            })
        })
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn coerce_set_rows(
    rows: Vec<Vec<Value>>,
    source: &[ColumnMeta],
    target: &[ColumnMeta],
) -> Result<Vec<Vec<Value>>> {
    coerce_set_rows_with_unknown(rows, source, target, None)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn coerce_set_rows_with_unknown(
    rows: Vec<Vec<Value>>,
    source: &[ColumnMeta],
    target: &[ColumnMeta],
    unknown: Option<&[bool]>,
) -> Result<Vec<Vec<Value>>> {
    if let Some(unknown) = unknown {
        assert_eq!(source.len(), unknown.len());
    }
    rows.into_iter()
        .map(|row| {
            row.into_iter()
                .zip(source)
                .zip(target)
                .enumerate()
                .map(|(index, ((value, source), target))| {
                    let target = PgType::create(
                        BaseType::resolve_oid(target.type_oid)
                            .expect("set-operation column has a supported type OID"),
                    );
                    if unknown.is_some_and(|unknown| unknown[index]) {
                        return match value {
                            Value::Null => Ok(Value::Null),
                            Value::Text(text) => coercion::coerce_unknown(
                                &text,
                                target,
                                CastContext::Implicit,
                                "UTC",
                            ),
                            _ => unreachable!("unknown set columns contain text or NULL"),
                        };
                    }
                    coercion::coerce(
                        value,
                        BaseType::resolve_oid(source.type_oid)
                            .expect("set-operation column has a supported type OID"),
                        target,
                        CastContext::Implicit,
                        "UTC",
                    )
                })
                .collect()
        })
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn remove_set_duplicates(rows: Vec<Vec<Value>>) -> Result<Vec<Vec<Value>>> {
    let mut selected: Vec<Vec<Value>> = Vec::new();
    for row in rows {
        let mut duplicate = false;
        for existing in &selected {
            if are_rows_not_distinct(existing, &row)? {
                duplicate = true;
                break;
            }
        }
        if !duplicate {
            selected.push(row);
        }
    }
    Ok(selected)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn select_set_intersection(
    left: Vec<Vec<Value>>,
    right: Vec<Vec<Value>>,
) -> Result<Vec<Vec<Value>>> {
    let mut consumed = vec![false; right.len()];
    let mut selected = Vec::new();
    for row in left {
        let mut match_index = None;
        for (index, candidate) in right.iter().enumerate() {
            if !consumed[index] && are_rows_not_distinct(&row, candidate)? {
                match_index = Some(index);
                break;
            }
        }
        if let Some(index) = match_index {
            consumed[index] = true;
            selected.push(row);
        }
    }
    Ok(selected)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn select_set_difference(left: Vec<Vec<Value>>, right: Vec<Vec<Value>>) -> Result<Vec<Vec<Value>>> {
    let mut consumed = vec![false; right.len()];
    let mut selected = Vec::new();
    for row in left {
        let mut match_index = None;
        for (index, candidate) in right.iter().enumerate() {
            if !consumed[index] && are_rows_not_distinct(&row, candidate)? {
                match_index = Some(index);
                break;
            }
        }
        if let Some(index) = match_index {
            consumed[index] = true;
        } else {
            selected.push(row);
        }
    }
    Ok(selected)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn sort_set_rows(
    rows: &mut [Vec<Value>],
    columns: &[ColumnMeta],
    query: &ast::Query,
) -> Result<()> {
    let Some(order_by) = &query.order_by else {
        return Ok(());
    };
    let ast::OrderByKind::Expressions(orders) = &order_by.kind else {
        return reject_unsupported("ORDER BY ALL is not implemented");
    };
    let orders = orders
        .iter()
        .map(|order| {
            let index = if let Some(position) = extract_number_literal(&order.expr)
                && !position.contains(['.', 'e', 'E'])
            {
                position
                    .parse::<usize>()
                    .ok()
                    .and_then(|position| position.checked_sub(1))
            } else if let ast::Expr::Identifier(identifier) = &order.expr {
                columns
                    .iter()
                    .position(|column| column.name == normalize_identifier(identifier))
            } else {
                None
            }
            .filter(|index| *index < columns.len())
            .ok_or_else(|| {
                PgError::create(
                    SqlState::InvalidColumnReference,
                    "ORDER BY position is not in select list",
                )
            })?;
            validate_ordering_type(
                BaseType::resolve_oid(columns[index].type_oid)
                    .expect("set-operation columns use supported PostgreSQL types"),
            )?;
            let ascending = resolve_order_ascending(&order.options)?;
            Ok((
                index,
                ascending,
                order.options.nulls_first.unwrap_or(!ascending),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(|left, right| {
        orders
            .iter()
            .find_map(|(index, ascending, nulls_first)| {
                let ordering = match (&left[*index], &right[*index]) {
                    (Value::Null, Value::Null) => Ordering::Equal,
                    (Value::Null, _) => {
                        if *nulls_first {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        }
                    }
                    (_, Value::Null) => {
                        if *nulls_first {
                            Ordering::Greater
                        } else {
                            Ordering::Less
                        }
                    }
                    (left, right) => {
                        let ordering = compare_values(left, right)
                            .expect("set-operation columns have one common type");
                        if *ascending {
                            ordering
                        } else {
                            ordering.reverse()
                        }
                    }
                };
                (ordering != Ordering::Equal).then_some(ordering)
            })
            .unwrap_or(Ordering::Equal)
    });
    Ok(())
}
