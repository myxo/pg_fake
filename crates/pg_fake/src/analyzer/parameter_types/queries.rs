use super::{
    super::scopes::is_projection_alias,
    expressions::{constrain_parameter_type, infer_expression_parameters},
};
use crate::{
    catalog::Catalog,
    coercion,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor,
    value::BaseType,
};
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn infer_query_parameters(
    query: &ast::Query,
    catalog: &Catalog,
    expected: Option<&[BaseType]>,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    if !matches!(query.body.as_ref(), ast::SetExpr::Select(_)) {
        return infer_set_expression_parameters(query.body.as_ref(), catalog, expected, types);
    }
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        unreachable!("set-expression shape was checked");
    };
    infer_from_parameters(&select.from, catalog, types)?;
    let bound = executor::bind_query_scope(catalog, select)?;
    let scope = executor::RowScope::Bound(&bound);
    if let Some(selection) = &select.selection {
        infer_expression_parameters(selection, scope, Some(BaseType::Bool), types)?;
    }
    let positional_expected = expected.filter(|expected| {
        expected.len() == select.projection.len()
            && select.projection.iter().all(|item| {
                matches!(
                    item,
                    ast::SelectItem::UnnamedExpr(_) | ast::SelectItem::ExprWithAlias { .. }
                )
            })
    });
    for (index, item) in select.projection.iter().enumerate() {
        if let ast::SelectItem::UnnamedExpr(expression)
        | ast::SelectItem::ExprWithAlias {
            expr: expression, ..
        } = item
        {
            infer_expression_parameters(
                expression,
                scope,
                positional_expected.map(|expected| expected[index]),
                types,
            )?;
        }
    }
    if let Some(order_by) = &query.order_by
        && let ast::OrderByKind::Expressions(orders) = &order_by.kind
    {
        for order in orders {
            if !is_projection_alias(&order.expr, &select.projection) {
                infer_expression_parameters(&order.expr, scope, None, types)?;
            }
        }
    }
    if let Some(ast::LimitClause::LimitOffset { limit, offset, .. }) = &query.limit_clause {
        if let Some(limit) = limit {
            infer_expression_parameters(limit, scope, Some(BaseType::Int8), types)?;
        }
        if let Some(offset) = offset {
            infer_expression_parameters(&offset.value, scope, Some(BaseType::Int8), types)?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn infer_set_expression_parameters(
    expression: &ast::SetExpr,
    catalog: &Catalog,
    expected: Option<&[BaseType]>,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    match expression {
        ast::SetExpr::Select(select) => {
            infer_from_parameters(&select.from, catalog, types)?;
            let bound = executor::bind_query_scope(catalog, select)?;
            let scope = executor::RowScope::Bound(&bound);
            if let Some(selection) = &select.selection {
                infer_expression_parameters(selection, scope, Some(BaseType::Bool), types)?;
            }
            for (index, item) in select.projection.iter().enumerate() {
                if let ast::SelectItem::UnnamedExpr(expression)
                | ast::SelectItem::ExprWithAlias {
                    expr: expression, ..
                } = item
                {
                    infer_expression_parameters(
                        expression,
                        scope,
                        expected.and_then(|expected| expected.get(index).copied()),
                        types,
                    )?;
                }
            }
            Ok(())
        }
        ast::SetExpr::Values(values) => {
            for row in &values.rows {
                for (index, expression) in row.iter().enumerate() {
                    infer_expression_parameters(
                        expression,
                        executor::RowScope::Table(&executor::create_constant_expression_schema()),
                        expected.and_then(|expected| expected.get(index).copied()),
                        types,
                    )?;
                }
            }
            Ok(())
        }
        ast::SetExpr::Query(query) => infer_query_parameters(query, catalog, expected, types),
        ast::SetExpr::SetOperation { left, right, .. } => {
            let left_types = infer_set_expression_types(left, catalog)?;
            let right_types = infer_set_expression_types(right, catalog)?;
            if left_types.len() != right_types.len() {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "each set-operation query must have the same number of columns",
                ));
            }
            let targets = left_types
                .into_iter()
                .zip(right_types)
                .enumerate()
                .map(|(index, (left, right))| match (left, right) {
                    (Some(left), Some(right)) => coercion::resolve_common_type(left, right)
                        .ok_or_else(|| {
                            PgError::create(
                                SqlState::DatatypeMismatch,
                                "set-operation types cannot be matched",
                            )
                        }),
                    (Some(data_type), None) | (None, Some(data_type)) => Ok(data_type),
                    (None, None) => Ok(expected
                        .and_then(|expected| expected.get(index).copied())
                        .unwrap_or(BaseType::Text)),
                })
                .collect::<Result<Vec<_>>>()?;
            infer_set_expression_parameters(left, catalog, Some(&targets), types)?;
            infer_set_expression_parameters(right, catalog, Some(&targets), types)
        }
        _ => reject_unsupported("set-operation input is not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn infer_from_parameters(
    from: &[ast::TableWithJoins],
    catalog: &Catalog,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    for table in from {
        infer_table_factor_parameters(&table.relation, catalog, types)?;
        for join in &table.joins {
            infer_table_factor_parameters(&join.relation, catalog, types)?;
        }
    }
    let mut visible = executor::create_value_scope(std::iter::empty());
    for table in from {
        infer_join_expression_parameters(table, catalog, &mut visible, types)?;
    }
    Ok(())
}

fn infer_join_expression_parameters(
    table: &ast::TableWithJoins,
    catalog: &Catalog,
    visible: &mut executor::BoundScope,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    let left_start = visible.count_columns();
    for (index, factor) in std::iter::once(&table.relation)
        .chain(table.joins.iter().map(|j| &j.relation))
        .enumerate()
    {
        if let ast::TableFactor::Derived {
            lateral: true,
            subquery,
            ..
        } = factor
        {
            let (query, _) = executor::bind_lateral_query(
                catalog,
                subquery,
                visible,
                &vec![crate::value::Value::Null; visible.count_columns()],
            )?;
            infer_query_parameters(&query, catalog, None, types)?;
        }
        if let ast::TableFactor::NestedJoin {
            table_with_joins, ..
        } = factor
        {
            infer_join_expression_parameters(
                table_with_joins,
                catalog,
                &mut visible.clone(),
                types,
            )?;
        }
        if let Some(executor::JsonTableFunction { name, argument, .. }) =
            executor::extract_json_table_function(factor)?
        {
            let base = executor::resolve_json_function_arguments(&name).expect("JSON expansion")[0];
            infer_expression_parameters(
                argument,
                executor::RowScope::Bound(visible),
                Some(base),
                types,
            )?;
        }
        if index == 0 {
            executor::bind_table_factor(catalog, factor, visible)?;
        } else {
            executor::bind_join(catalog, &table.joins[index - 1], visible, left_start)?;
        }
    }
    let scope = executor::RowScope::Bound(visible);
    for join in &table.joins {
        let constraint = match &join.join_operator {
            ast::JoinOperator::Join(c)
            | ast::JoinOperator::Inner(c)
            | ast::JoinOperator::CrossJoin(c)
            | ast::JoinOperator::Left(c)
            | ast::JoinOperator::LeftOuter(c)
            | ast::JoinOperator::Right(c)
            | ast::JoinOperator::RightOuter(c)
            | ast::JoinOperator::FullOuter(c) => c,
            _ => continue,
        };
        if let ast::JoinConstraint::On(expression) = constraint {
            infer_expression_parameters(expression, scope, Some(BaseType::Bool), types)?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn infer_table_factor_parameters(
    factor: &ast::TableFactor,
    catalog: &Catalog,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    if let Some(executor::JsonTableFunction { name, argument, .. }) =
        executor::extract_json_table_function(factor)?
    {
        let base = executor::resolve_json_function_arguments(&name).expect("JSON expansion")[0];
        constrain_parameter_type(argument, Some(base), types)?;
    }
    match factor {
        ast::TableFactor::Derived {
            lateral: false,
            subquery,
            ..
        } => infer_query_parameters(subquery, catalog, None, types),
        ast::TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            infer_table_factor_parameters(&table_with_joins.relation, catalog, types)?;
            for join in &table_with_joins.joins {
                infer_table_factor_parameters(&join.relation, catalog, types)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn infer_set_expression_types(
    expression: &ast::SetExpr,
    catalog: &Catalog,
) -> Result<Vec<Option<BaseType>>> {
    match expression {
        ast::SetExpr::Select(select) => {
            let scope = executor::bind_query_scope(catalog, select)?;
            let types = select
                .projection
                .iter()
                .flat_map(|item| match item {
                    ast::SelectItem::Wildcard(_) => Vec::new(),
                    ast::SelectItem::UnnamedExpr(expression)
                    | ast::SelectItem::ExprWithAlias {
                        expr: expression, ..
                    } => vec![
                        if executor::extract_unknown_string_literal(expression).is_some()
                            || executor::is_null_literal(expression)
                            || matches!(expression, ast::Expr::Value(value)
                                if matches!(value.value, ast::Value::Placeholder(_)))
                        {
                            None
                        } else {
                            executor::infer_expression_type(
                                expression,
                                executor::RowScope::Bound(&scope),
                            )
                            .ok()
                        },
                    ],
                    _ => Vec::new(),
                })
                .collect::<Vec<_>>();
            let unknown = executor::identify_unknown_set_operand_columns(expression, types.len());
            let parameters = identify_parameter_set_operand_columns(expression, types.len());
            Ok(types
                .into_iter()
                .zip(unknown.into_iter().zip(parameters))
                .map(|(data_type, (unknown, parameter))| {
                    data_type.or((!unknown && !parameter).then_some(BaseType::Text))
                })
                .collect())
        }
        ast::SetExpr::Values(values) => {
            let width = values.rows.first().map(|row| row.len()).unwrap_or(0);
            if values.rows.iter().any(|row| row.len() != width) {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "VALUES lists must all be the same length",
                ));
            }
            Ok((0..width)
                .map(|index| {
                    values.rows.iter().fold(None, |common, row| {
                        let data_type = executor::infer_expression_type(
                            &row[index],
                            executor::RowScope::Table(
                                &executor::create_constant_expression_schema(),
                            ),
                        )
                        .ok()?;
                        Some(match common {
                            Some(common) => coercion::resolve_common_type(common, data_type)?,
                            None => data_type,
                        })
                    })
                })
                .collect())
        }
        ast::SetExpr::Query(query) => {
            let types = infer_set_expression_types(&query.body, catalog)?;
            let unknown = executor::identify_unknown_set_operand_columns(expression, types.len());
            let parameters = identify_parameter_set_operand_columns(expression, types.len());
            Ok(types
                .into_iter()
                .zip(unknown.into_iter().zip(parameters))
                .map(|(data_type, (unknown, parameter))| {
                    data_type.or((!unknown && !parameter).then_some(BaseType::Text))
                })
                .collect())
        }
        ast::SetExpr::SetOperation { left, right, .. } => {
            let left = infer_set_expression_types(left, catalog)?;
            let right = infer_set_expression_types(right, catalog)?;
            if left.len() != right.len() {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "each set-operation query must have the same number of columns",
                ));
            }
            Ok(left
                .into_iter()
                .zip(right)
                .map(|(left, right)| match (left, right) {
                    (Some(left), Some(right)) => coercion::resolve_common_type(left, right),
                    (Some(data_type), None) | (None, Some(data_type)) => Some(data_type),
                    (None, None) => None,
                })
                .collect())
        }
        _ => reject_unsupported("set-operation input is not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn identify_parameter_set_operand_columns(expression: &ast::SetExpr, columns: usize) -> Vec<bool> {
    let select = match expression {
        ast::SetExpr::Select(select) => select.as_ref(),
        ast::SetExpr::Query(query) => match query.body.as_ref() {
            ast::SetExpr::Select(select) => select.as_ref(),
            _ => return vec![false; columns],
        },
        _ => return vec![false; columns],
    };
    let parameters = select
        .projection
        .iter()
        .map(|item| match item {
            ast::SelectItem::UnnamedExpr(ast::Expr::Value(value))
            | ast::SelectItem::ExprWithAlias {
                expr: ast::Expr::Value(value),
                ..
            } => matches!(value.value, ast::Value::Placeholder(_)),
            _ => false,
        })
        .collect::<Vec<_>>();
    if parameters.len() == columns {
        parameters
    } else {
        vec![false; columns]
    }
}
