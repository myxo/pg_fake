//! Parameter analysis and binding for prepared statements.

use std::ops::ControlFlow;

use sqlparser::ast;

use crate::{
    catalog::{Catalog, TableSchema},
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    executor,
    value::{BaseType, PgType, Value},
};

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn infer_returning_parameters(
    returning: Option<&[ast::SelectItem]>,
    scope: executor::RowScope<'_>,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    let Some(returning) = returning else {
        return Ok(());
    };
    for item in returning {
        if let ast::SelectItem::UnnamedExpr(expression)
        | ast::SelectItem::ExprWithAlias {
            expr: expression, ..
        } = item
        {
            infer_expression_parameters(expression, scope, None, types)?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validate_returning_items(
    returning: Option<&[ast::SelectItem]>,
    scope: executor::RowScope<'_>,
) -> Result<()> {
    let Some(returning) = returning else {
        return Ok(());
    };
    for item in returning {
        if let ast::SelectItem::UnnamedExpr(expression)
        | ast::SelectItem::ExprWithAlias {
            expr: expression, ..
        } = item
        {
            executor::infer_expression_type(expression, scope)?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn get_table_alias(table: &ast::TableFactor) -> Option<&ast::Ident> {
    let ast::TableFactor::Table { alias, .. } = table else {
        return None;
    };
    alias.as_ref().map(|alias| &alias.name)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn get_update_from(update: &ast::Update) -> Result<&[ast::TableWithJoins]> {
    match &update.from {
        None => Ok(&[]),
        Some(ast::UpdateTableFromKind::AfterSet(from)) => Ok(from),
        Some(ast::UpdateTableFromKind::BeforeSet(_)) => {
            reject_unsupported("UPDATE FROM before SET is not implemented")
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn bind_update_scope(update: &ast::Update, catalog: &Catalog) -> Result<executor::BoundScope> {
    let schema = resolve_table_schema(&update.table.relation, catalog)?;
    Ok(executor::combine_bound_scopes(
        executor::bind_target_scope(schema, get_table_alias(&update.table.relation)),
        executor::bind_from_scope(catalog, get_update_from(update)?)?,
    ))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn bind_delete_scope(
    delete: &ast::Delete,
    schema: &TableSchema,
    target: &ast::TableFactor,
    catalog: &Catalog,
) -> Result<executor::BoundScope> {
    Ok(executor::combine_bound_scopes(
        executor::bind_target_scope(schema, get_table_alias(target)),
        executor::bind_from_scope(catalog, delete.using.as_deref().unwrap_or_default())?,
    ))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn infer_query_parameters(
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
    let bound = executor::bind_from_scope(catalog, from)?;
    let scope = executor::RowScope::Bound(&bound);
    for table in from {
        for join in &table.joins {
            let constraint = match &join.join_operator {
                ast::JoinOperator::Join(constraint)
                | ast::JoinOperator::Inner(constraint)
                | ast::JoinOperator::CrossJoin(constraint)
                | ast::JoinOperator::Left(constraint)
                | ast::JoinOperator::LeftOuter(constraint)
                | ast::JoinOperator::Right(constraint)
                | ast::JoinOperator::RightOuter(constraint)
                | ast::JoinOperator::FullOuter(constraint) => constraint,
                _ => continue,
            };
            if let ast::JoinConstraint::On(expression) = constraint {
                infer_expression_parameters(expression, scope, Some(BaseType::Bool), types)?;
            }
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
    match factor {
        ast::TableFactor::Derived { subquery, .. } => {
            infer_query_parameters(subquery, catalog, None, types)
        }
        ast::TableFactor::NestedJoin {
            table_with_joins, ..
        } => infer_from_parameters(std::slice::from_ref(table_with_joins), catalog, types),
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
            Ok(select
                .projection
                .iter()
                .flat_map(|item| match item {
                    ast::SelectItem::Wildcard(_) => Vec::new(),
                    ast::SelectItem::UnnamedExpr(expression)
                    | ast::SelectItem::ExprWithAlias {
                        expr: expression, ..
                    } => vec![
                        executor::infer_expression_type(
                            expression,
                            executor::RowScope::Bound(&scope),
                        )
                        .ok(),
                    ],
                    _ => Vec::new(),
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
        ast::SetExpr::Query(query) => infer_set_expression_types(&query.body, catalog),
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
fn constrain_statement_parameters(
    statement: &ast::Statement,
    catalog: &Catalog,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    match statement {
        ast::Statement::Insert(insert) => {
            let schema = catalog
                .require_named_table(&executor::resolve_insert_table_name(&insert.table)?)?;
            let columns = if insert.columns.is_empty() {
                (0..schema.columns.len()).collect::<Vec<_>>()
            } else {
                insert
                    .columns
                    .iter()
                    .map(|name| {
                        let name = executor::normalize_unqualified_object_name(name)?;
                        schema
                            .columns
                            .iter()
                            .position(|column| column.name == name)
                            .ok_or_else(|| {
                                PgError::create(
                                    SqlState::UndefinedColumn,
                                    format!("column {:?} does not exist", name),
                                )
                            })
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            if let Some(source) = &insert.source {
                if let ast::SetExpr::Values(values) = source.body.as_ref() {
                    for row in &values.rows {
                        if row.len() != columns.len() {
                            return Err(PgError::create(
                                SqlState::SyntaxError,
                                "INSERT has wrong number of values",
                            ));
                        }
                        for (expression, column) in row.iter().zip(&columns) {
                            infer_expression_parameters(
                                expression,
                                executor::RowScope::Table(
                                    &executor::create_constant_expression_schema(),
                                ),
                                Some(schema.columns[*column].data_type.base),
                                types,
                            )?;
                        }
                    }
                } else {
                    let expected = columns
                        .iter()
                        .map(|column| schema.columns[*column].data_type.base)
                        .collect::<Vec<_>>();
                    infer_query_parameters(source, catalog, Some(&expected), types)?;
                }
            }
            let returning_scope = executor::bind_target_scope(
                schema,
                insert.table_alias.as_ref().map(|alias| &alias.alias),
            );
            infer_returning_parameters(
                insert.returning.as_deref(),
                executor::RowScope::Bound(&returning_scope),
                types,
            )?;
        }
        ast::Statement::Update(update) => {
            let schema = resolve_table_schema(&update.table.relation, catalog)?;
            let bound = bind_update_scope(update, catalog)?;
            let scope = executor::RowScope::Bound(&bound);
            for assignment in &update.assignments {
                let ast::AssignmentTarget::ColumnName(name) = &assignment.target else {
                    continue;
                };
                let name = executor::normalize_unqualified_object_name(name)?;
                let column = schema
                    .columns
                    .iter()
                    .find(|column| column.name == name)
                    .ok_or_else(|| {
                        PgError::create(
                            SqlState::UndefinedColumn,
                            format!("column {name:?} does not exist"),
                        )
                    })?;
                infer_expression_parameters(
                    &assignment.value,
                    scope,
                    Some(column.data_type.base),
                    types,
                )?;
            }
            if let Some(selection) = &update.selection {
                infer_expression_parameters(selection, scope, Some(BaseType::Bool), types)?;
            }
            infer_returning_parameters(update.returning.as_deref(), scope, types)?;
        }
        ast::Statement::Delete(delete) => {
            let ast::FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(());
            };
            if let Some(first) = from.first() {
                let schema = resolve_table_schema(&first.relation, catalog)?;
                let bound = bind_delete_scope(delete, schema, &first.relation, catalog)?;
                let scope = executor::RowScope::Bound(&bound);
                if let Some(selection) = &delete.selection {
                    infer_expression_parameters(selection, scope, Some(BaseType::Bool), types)?;
                }
                infer_returning_parameters(delete.returning.as_deref(), scope, types)?;
            }
        }
        ast::Statement::Query(query) => infer_query_parameters(query, catalog, None, types)?,
        _ => {}
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn infer_parameter_types_with_data_modifying_ctes(
    described: &ast::Statement,
    data_modifying_ctes: &[ast::Statement],
    catalog: &Catalog,
    parameter_count: usize,
) -> Result<Vec<BaseType>> {
    let mut types = vec![None; parameter_count];
    for statement in data_modifying_ctes {
        constrain_statement_parameters(statement, catalog, &mut types)?;
    }
    constrain_statement_parameters(described, catalog, &mut types)?;
    let types = finalize_parameter_types(types);
    let bound = bind_parameters(described, &types, &vec![Value::Null; types.len()])?;
    validate_statement(&bound, catalog)?;
    for statement in data_modifying_ctes {
        let bound = bind_parameters(statement, &types, &vec![Value::Null; types.len()])?;
        validate_statement(&bound, catalog)?;
    }
    Ok(types)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn substitute_typed_subqueries(
    statement: &ast::Statement,
    catalog: &Catalog,
) -> Result<ast::Statement> {
    let mut statement = statement.clone();
    if let ast::Statement::Insert(insert) = &mut statement {
        if let Some(source) = &mut insert.source
            && let ast::SetExpr::Select(select) = source.body.as_ref()
        {
            let outer = executor::bind_query_scope(catalog, select)?;
            substitute_scoped_subqueries(source.as_mut(), catalog, &outer)?;
        }
        if let Some(returning) = &mut insert.returning {
            let schema = catalog
                .require_named_table(&executor::resolve_insert_table_name(&insert.table)?)?;
            let outer = executor::bind_target_scope(
                schema,
                insert.table_alias.as_ref().map(|alias| &alias.alias),
            );
            substitute_scoped_subqueries(returning, catalog, &outer)?;
        }
        return Ok(statement);
    }
    let outer = match &statement {
        ast::Statement::Query(query) => match query.body.as_ref() {
            ast::SetExpr::Select(select) => Some(executor::bind_query_scope(catalog, select)?),
            _ => None,
        },
        ast::Statement::Update(update) => Some(bind_update_scope(update, catalog)?),
        ast::Statement::Delete(delete) => {
            let ast::FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(statement);
            };
            match from.first() {
                Some(first) => {
                    let schema = resolve_table_schema(&first.relation, catalog)?;
                    Some(bind_delete_scope(delete, schema, &first.relation, catalog)?)
                }
                None => None,
            }
        }
        _ => None,
    };
    if let Some(outer) = &outer {
        substitute_scoped_subqueries(&mut statement, catalog, outer)?;
        return Ok(statement);
    }
    let mut error = None;
    let _ = ast::visit_expressions_mut(&mut statement, |expression| {
        if error.is_some() {
            return ControlFlow::Break(());
        }
        let result = match expression {
            ast::Expr::Subquery(query) => executor::infer_query_output_columns(catalog, query)
                .and_then(|columns| {
                    if columns.len() != 1 {
                        return Err(PgError::create(
                            SqlState::SyntaxError,
                            "subquery must return only one column",
                        ));
                    }
                    Ok(create_typed_literal(Value::Null, columns[0].1))
                }),
            ast::Expr::Exists { .. } => Ok(create_typed_literal(
                Value::Bool(false),
                PgType::create(BaseType::Bool),
            )),
            ast::Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => executor::infer_query_output_columns(catalog, subquery).and_then(|columns| {
                let left_width = match expr.as_ref() {
                    ast::Expr::Tuple(fields) => fields.len(),
                    _ => 1,
                };
                if columns.len() != left_width {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let fields = columns
                    .into_iter()
                    .map(|(_, data_type)| create_typed_literal(Value::Null, data_type))
                    .collect::<Vec<_>>();
                Ok(ast::Expr::InList {
                    expr: expr.clone(),
                    list: vec![if fields.len() == 1 {
                        fields.into_iter().next().expect("subquery has one column")
                    } else {
                        ast::Expr::Tuple(fields)
                    }],
                    negated: *negated,
                })
            }),
            _ => return ControlFlow::Continue(()),
        };
        match result {
            Ok(value) => *expression = value,
            Err(describe_error) => error = Some(describe_error),
        }
        ControlFlow::Continue(())
    });
    error.map_or(Ok(statement), Err)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn substitute_scoped_subqueries<V: ast::VisitMut>(
    value: &mut V,
    catalog: &Catalog,
    outer: &executor::BoundScope,
) -> Result<()> {
    let mut describer = TypedSubquerySubstituter {
        catalog,
        outer,
        error: None,
    };
    let _ = value.visit(&mut describer);
    describer.error.map_or(Ok(()), Err)
}

struct TypedSubquerySubstituter<'a> {
    catalog: &'a Catalog,
    outer: &'a executor::BoundScope,
    error: Option<PgError>,
}

impl ast::VisitorMut for TypedSubquerySubstituter<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> ControlFlow<Self::Break> {
        if self.error.is_some() {
            return ControlFlow::Break(());
        }
        if !matches!(
            expression,
            ast::Expr::Subquery(_)
                | ast::Expr::Exists { .. }
                | ast::Expr::InSubquery { .. }
                | ast::Expr::AnyOp { .. }
                | ast::Expr::AllOp { .. }
        ) {
            return ControlFlow::Continue(());
        }
        match executor::substitute_typed_subqueries(self.catalog, expression, self.outer) {
            Ok(described) => *expression = described,
            Err(error) => {
                self.error = Some(error);
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn coerce_parameters(
    parameter_types: &[BaseType],
    values: &[Value],
) -> Result<Vec<Value>> {
    if values.len() != parameter_types.len() {
        return Err(PgError::create(
            SqlState::ProtocolViolation,
            format!(
                "bind message supplies {} parameters, but prepared statement requires {}",
                values.len(),
                parameter_types.len()
            ),
        ));
    }
    values
        .iter()
        .cloned()
        .zip(parameter_types.iter().copied())
        .map(|(value, target)| coerce_parameter(value, target))
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn bind_parameters(
    statement: &ast::Statement,
    infer_parameter_types: &[BaseType],
    values: &[Value],
) -> Result<ast::Statement> {
    let values = coerce_parameters(infer_parameter_types, values)?;
    let mut statement = statement.clone();
    let mut error = None;
    let _ = ast::visit_expressions_mut(&mut statement, |expression| {
        let ast::Expr::Value(value) = expression else {
            return ControlFlow::Continue(());
        };
        let ast::Value::Placeholder(placeholder) = &value.value else {
            return ControlFlow::Continue(());
        };
        let index = match parse_placeholder_index(placeholder) {
            Ok(index) => index,
            Err(bind_error) => {
                error = Some(bind_error);
                return ControlFlow::Break(());
            }
        };
        let target = infer_parameter_types[index];
        *expression = create_typed_literal(values[index].clone(), PgType::create(target));
        ControlFlow::Continue(())
    });
    error.map_or(Ok(statement), Err)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn count_parameters(statement: &ast::Statement) -> Result<usize> {
    let mut maximum = 0;
    let mut error = None;
    let _ = ast::visit_expressions(statement, |expression| {
        let ast::Expr::Value(value) = expression else {
            return ControlFlow::Continue(());
        };
        let ast::Value::Placeholder(placeholder) = &value.value else {
            return ControlFlow::Continue(());
        };
        match parse_placeholder_index(placeholder) {
            Ok(index) => maximum = maximum.max(index + 1),
            Err(parameter_error) => {
                error = Some(parameter_error);
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    });
    error.map_or(Ok(maximum), Err)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn parse_placeholder_index(placeholder: &str) -> Result<usize> {
    let index = placeholder
        .strip_prefix('$')
        .and_then(|index| index.parse::<usize>().ok())
        .filter(|index| *index > 0)
        .ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedParameter,
                format!("there is no parameter {placeholder}"),
            )
        })?;
    Ok(index - 1)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_table_schema<'a>(
    factor: &ast::TableFactor,
    catalog: &'a Catalog,
) -> Result<&'a TableSchema> {
    let ast::TableFactor::Table {
        name, args: None, ..
    } = factor
    else {
        return reject_unsupported("table source is not implemented");
    };
    catalog.require_named_table(&executor::normalize_relation_name(name)?)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn infer_expression_parameters(
    expression: &ast::Expr,
    schema: executor::RowScope<'_>,
    expected: Option<BaseType>,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    constrain_parameter_type(expression, expected, types)?;
    let mut error = None;
    let _ = ast::visit_expressions(expression, |expression| {
        let result = match expression {
            ast::Expr::Identifier(identifier)
                if identifier.quote_style.is_none()
                    && identifier.value.eq_ignore_ascii_case("default") =>
            {
                Ok(())
            }
            ast::Expr::Identifier(_) => {
                executor::infer_expression_type(expression, schema).map(|_| ())
            }
            ast::Expr::Nested(inner) => constrain_parameter_type(inner, expected, types),
            ast::Expr::Cast {
                expr, data_type, ..
            } => coercion::convert_ast_data_type(data_type)
                .and_then(|target| constrain_parameter_type(expr, Some(target.base), types)),
            ast::Expr::UnaryOp { op, expr } => constrain_parameter_type(
                expr,
                matches!(op, ast::UnaryOperator::Not).then_some(BaseType::Bool),
                types,
            ),
            ast::Expr::BinaryOp { left, op, right } => {
                let boolean = matches!(op, ast::BinaryOperator::And | ast::BinaryOperator::Or);
                let left_expected = if boolean {
                    Some(BaseType::Bool)
                } else {
                    executor::infer_expression_type(right, schema).ok()
                };
                let right_expected = if boolean {
                    Some(BaseType::Bool)
                } else {
                    executor::infer_expression_type(left, schema).ok()
                };
                constrain_parameter_type(left, left_expected, types)
                    .and_then(|()| constrain_parameter_type(right, right_expected, types))
            }
            ast::Expr::InList { expr, list, .. } => (|| {
                let left = match expr.as_ref() {
                    ast::Expr::Tuple(fields) => fields.as_slice(),
                    expr => std::slice::from_ref(expr),
                };
                for candidate in list {
                    let right = match candidate {
                        ast::Expr::Tuple(fields) => fields.as_slice(),
                        candidate => std::slice::from_ref(candidate),
                    };
                    if left.len() != right.len() {
                        return Err(PgError::create(
                            SqlState::SyntaxError,
                            "subquery has too many columns",
                        ));
                    }
                    for (left, right) in left.iter().zip(right) {
                        constrain_parameter_type(
                            left,
                            executor::infer_expression_type(right, schema).ok(),
                            types,
                        )?;
                        constrain_parameter_type(
                            right,
                            executor::infer_expression_type(left, schema).ok(),
                            types,
                        )?;
                    }
                }
                Ok(())
            })(),
            ast::Expr::AnyOp { left, right, .. } | ast::Expr::AllOp { left, right, .. } => {
                constrain_parameter_type(
                    left,
                    executor::infer_expression_type(right, schema).ok(),
                    types,
                )
                .and_then(|()| {
                    constrain_parameter_type(
                        right,
                        executor::infer_expression_type(left, schema).ok(),
                        types,
                    )
                })
            }
            ast::Expr::IsTrue(inner)
            | ast::Expr::IsFalse(inner)
            | ast::Expr::IsUnknown(inner)
            | ast::Expr::IsNotTrue(inner)
            | ast::Expr::IsNotFalse(inner)
            | ast::Expr::IsNotUnknown(inner) => {
                constrain_parameter_type(inner, Some(BaseType::Bool), types)
            }
            ast::Expr::Function(function) => infer_function_parameters(function, schema, types),
            _ => Ok(()),
        };
        if let Err(infer_error) = result {
            error = Some(infer_error);
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    error.map_or(Ok(()), Err)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn infer_function_parameters(
    function: &ast::Function,
    schema: executor::RowScope<'_>,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    let ast::FunctionArguments::List(list) = &function.args else {
        return Ok(());
    };
    let arguments = list
        .args
        .iter()
        .filter_map(|argument| match argument {
            ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expression)) => Some(expression),
            _ => None,
        })
        .collect::<Vec<_>>();
    let name = executor::normalize_unqualified_object_name(&function.name)?;
    if matches!(name.as_str(), "nextval" | "currval") {
        for argument in arguments {
            constrain_parameter_type(argument, Some(BaseType::Text), types)?;
        }
        return Ok(());
    }
    if name == "setval" {
        for (argument, expected) in
            arguments
                .iter()
                .zip([BaseType::Text, BaseType::Int8, BaseType::Bool])
        {
            constrain_parameter_type(argument, Some(expected), types)?;
        }
        return Ok(());
    }
    let expected = match name.as_str() {
        "length" | "lower" | "upper" => Some(BaseType::Text),
        _ => arguments
            .iter()
            .find_map(|argument| executor::infer_expression_type(argument, schema).ok()),
    };
    for argument in arguments {
        constrain_parameter_type(argument, expected, types)?;
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn constrain_parameter_type(
    expression: &ast::Expr,
    expected: Option<BaseType>,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let expression = match expression {
        ast::Expr::Nested(inner) => inner.as_ref(),
        expression => expression,
    };
    let ast::Expr::Value(value) = expression else {
        return Ok(());
    };
    let ast::Value::Placeholder(placeholder) = &value.value else {
        return Ok(());
    };
    let index = parse_placeholder_index(placeholder)?;
    let slot = &mut types[index];
    if let Some(previous) = *slot
        && previous != expected
    {
        let Some(common) = coercion::resolve_common_type(previous, expected) else {
            return Err(PgError::create(
                SqlState::AmbiguousParameter,
                format!("inconsistent types deduced for parameter {placeholder}"),
            ));
        };
        *slot = Some(common);
    } else {
        *slot = Some(expected);
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn finalize_parameter_types(types: Vec<Option<BaseType>>) -> Vec<BaseType> {
    types
        .into_iter()
        .map(|data_type| data_type.unwrap_or(BaseType::Text))
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validate_statement(statement: &ast::Statement, catalog: &Catalog) -> Result<()> {
    match statement {
        ast::Statement::Insert(insert) => {
            let schema = catalog
                .require_named_table(&executor::resolve_insert_table_name(&insert.table)?)?;
            let columns = if insert.columns.is_empty() {
                (0..schema.columns.len()).collect::<Vec<_>>()
            } else {
                insert
                    .columns
                    .iter()
                    .map(|name| {
                        let name = executor::normalize_unqualified_object_name(name)?;
                        schema
                            .columns
                            .iter()
                            .position(|column| column.name == name)
                            .ok_or_else(|| {
                                PgError::create(
                                    SqlState::UndefinedColumn,
                                    format!("column {:?} does not exist", name),
                                )
                            })
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            if let Some(source) = &insert.source {
                if let ast::SetExpr::Values(values) = source.body.as_ref() {
                    for row in &values.rows {
                        if row.len() != columns.len() {
                            return Err(PgError::create(
                                SqlState::SyntaxError,
                                "INSERT has wrong number of values",
                            ));
                        }
                        for (expression, column) in row.iter().zip(&columns) {
                            validate_assignment(
                                expression,
                                schema.columns[*column].data_type,
                                executor::RowScope::Table(
                                    &executor::create_constant_expression_schema(),
                                ),
                            )?;
                        }
                    }
                } else {
                    validate_statement(&ast::Statement::Query(source.clone()), catalog)?;
                    let output = executor::infer_query_output_columns(catalog, source)?;
                    if output.len() != columns.len() {
                        return Err(PgError::create(
                            SqlState::SyntaxError,
                            "INSERT has wrong number of values",
                        ));
                    }
                    let unknown_columns =
                        executor::identify_unknown_query_columns(source, columns.len());
                    for (((_, source_type), unknown), column) in
                        output.iter().zip(&unknown_columns).zip(&columns)
                    {
                        if !*unknown
                            && !coercion::can_cast(
                                source_type.base,
                                schema.columns[*column].data_type.base,
                                CastContext::Assignment,
                            )
                        {
                            return Err(PgError::create(
                                SqlState::DatatypeMismatch,
                                "column has incompatible type",
                            ));
                        }
                    }
                }
            }
            let returning_scope = executor::bind_target_scope(
                schema,
                insert.table_alias.as_ref().map(|alias| &alias.alias),
            );
            validate_returning_items(
                insert.returning.as_deref(),
                executor::RowScope::Bound(&returning_scope),
            )?;
        }
        ast::Statement::Update(update) => {
            let schema = resolve_table_schema(&update.table.relation, catalog)?;
            let bound = bind_update_scope(update, catalog)?;
            let scope = executor::RowScope::Bound(&bound);
            if let Some(selection) = &update.selection {
                validate_boolean(selection, scope, "WHERE requires a boolean expression")?;
            }
            for assignment in &update.assignments {
                let ast::AssignmentTarget::ColumnName(name) = &assignment.target else {
                    continue;
                };
                let name = executor::normalize_unqualified_object_name(name)?;
                let column = schema
                    .columns
                    .iter()
                    .find(|column| column.name == name)
                    .ok_or_else(|| {
                        PgError::create(
                            SqlState::UndefinedColumn,
                            format!("column {name:?} does not exist"),
                        )
                    })?;
                validate_assignment(&assignment.value, column.data_type, scope)?;
            }
            validate_returning_items(update.returning.as_deref(), scope)?;
        }
        ast::Statement::Delete(delete) => {
            let ast::FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(());
            };
            if let Some(first) = from.first() {
                let schema = resolve_table_schema(&first.relation, catalog)?;
                let bound = bind_delete_scope(delete, schema, &first.relation, catalog)?;
                let scope = executor::RowScope::Bound(&bound);
                if let Some(selection) = &delete.selection {
                    validate_boolean(selection, scope, "WHERE requires a boolean expression")?;
                }
                validate_returning_items(delete.returning.as_deref(), scope)?;
            }
        }
        ast::Statement::Query(query) => {
            let ast::SetExpr::Select(select) = query.body.as_ref() else {
                return Ok(());
            };
            let bound = executor::bind_query_scope(catalog, select)?;
            let schema = executor::RowScope::Bound(&bound);
            if let Some(selection) = &select.selection {
                validate_boolean(selection, schema, "WHERE requires a boolean expression")?;
            }
            for item in &select.projection {
                match item {
                    ast::SelectItem::UnnamedExpr(expression)
                    | ast::SelectItem::ExprWithAlias {
                        expr: expression, ..
                    } => {
                        executor::infer_expression_type(expression, schema)?;
                    }
                    _ => {}
                }
            }
            if let Some(order_by) = &query.order_by
                && let ast::OrderByKind::Expressions(orders) = &order_by.kind
            {
                for order in orders {
                    if !is_projection_alias(&order.expr, &select.projection) {
                        executor::infer_expression_type(&order.expr, schema)?;
                    }
                }
            }
            if let Some(ast::LimitClause::LimitOffset { limit, offset, .. }) = &query.limit_clause {
                if let Some(limit) = limit
                    && !matches!(limit, ast::Expr::Identifier(name) if name.value.eq_ignore_ascii_case("all"))
                {
                    validate_implicit_type(limit, BaseType::Int8, schema)?;
                }
                if let Some(offset) = offset {
                    validate_implicit_type(&offset.value, BaseType::Int8, schema)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_projection_alias(expression: &ast::Expr, projection: &[ast::SelectItem]) -> bool {
    let ast::Expr::Identifier(identifier) = expression else {
        return false;
    };
    projection.iter().any(|item| {
        matches!(item, ast::SelectItem::ExprWithAlias { alias, .. }
            if executor::normalize_identifier(alias) == executor::normalize_identifier(identifier))
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validate_boolean(
    expression: &ast::Expr,
    schema: executor::RowScope<'_>,
    message: &str,
) -> Result<()> {
    let data_type = executor::infer_expression_type(expression, schema)?;
    if data_type == BaseType::Bool || executor::is_null_literal(expression) {
        Ok(())
    } else {
        Err(PgError::create(SqlState::DatatypeMismatch, message))
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validate_assignment(
    expression: &ast::Expr,
    target: PgType,
    schema: executor::RowScope<'_>,
) -> Result<()> {
    if matches!(expression, ast::Expr::Identifier(name) if name.value.eq_ignore_ascii_case("default"))
        || matches!(expression, ast::Expr::Value(value) if matches!(&value.value, ast::Value::SingleQuotedString(_)))
        || executor::is_null_literal(expression)
    {
        return Ok(());
    }
    let source = executor::infer_expression_type(expression, schema)?;
    if coercion::can_cast(source, target.base, CastContext::Assignment) {
        Ok(())
    } else {
        Err(PgError::create(
            SqlState::DatatypeMismatch,
            "column has incompatible type",
        ))
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validate_implicit_type(
    expression: &ast::Expr,
    target: BaseType,
    schema: executor::RowScope<'_>,
) -> Result<()> {
    if executor::is_null_literal(expression)
        || matches!(expression, ast::Expr::Value(value) if matches!(&value.value, ast::Value::SingleQuotedString(_)))
    {
        return Ok(());
    }
    let source = executor::infer_expression_type(expression, schema)?;
    if coercion::can_cast(source, target, CastContext::Implicit) {
        Ok(())
    } else {
        Err(PgError::create(
            SqlState::DatatypeMismatch,
            "parameter has incompatible type",
        ))
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn coerce_parameter(value: Value, target: BaseType) -> Result<Value> {
    let Some(source) = value.get_base_type() else {
        return Ok(Value::Null);
    };
    coercion::coerce(value, source, PgType::create(target), CastContext::Implicit)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn create_typed_literal(value: Value, data_type: PgType) -> ast::Expr {
    let literal = match value {
        Value::Null => ast::Value::Null,
        Value::Bool(value) => ast::Value::Boolean(value),
        Value::Int2(value) => ast::Value::Number(value.to_string(), false),
        Value::Int4(value) => ast::Value::Number(value.to_string(), false),
        Value::Int8(value) => ast::Value::Number(value.to_string(), false),
        Value::Float4(value) => {
            ast::Value::SingleQuotedString(Value::Float4(value).format_postgres_text())
        }
        Value::Float8(value) => {
            ast::Value::SingleQuotedString(Value::Float8(value).format_postgres_text())
        }
        Value::Numeric(value) => ast::Value::Number(value.to_plain_string(), false),
        Value::Text(value) => ast::Value::SingleQuotedString(value),
        Value::Bytea(value) => {
            ast::Value::SingleQuotedString(Value::Bytea(value).format_postgres_text())
        }
        Value::Uuid(value) => ast::Value::SingleQuotedString(value.to_string()),
        Value::Date(value) => {
            ast::Value::SingleQuotedString(Value::Date(value).format_postgres_text())
        }
        Value::Time(value) => {
            ast::Value::SingleQuotedString(Value::Time(value).format_postgres_text())
        }
        Value::Timestamp(value) => {
            ast::Value::SingleQuotedString(Value::Timestamp(value).format_postgres_text())
        }
        Value::TimestampTz(value) => {
            ast::Value::SingleQuotedString(Value::TimestampTz(value).format_postgres_text())
        }
        Value::Interval(value) => {
            ast::Value::SingleQuotedString(Value::Interval(value).format_postgres_text())
        }
    };
    ast::Expr::Cast {
        kind: ast::CastKind::Cast,
        expr: Box::new(ast::Expr::Value(literal.into())),
        data_type: convert_to_ast_data_type(data_type),
        array: false,
        format: None,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn convert_to_ast_data_type(data_type: PgType) -> ast::DataType {
    match data_type.base {
        BaseType::Bool => ast::DataType::Boolean,
        BaseType::Int2 => ast::DataType::SmallInt(None),
        BaseType::Int4 => ast::DataType::Integer(None),
        BaseType::Int8 => ast::DataType::BigInt(None),
        BaseType::Float4 => ast::DataType::Real,
        BaseType::Float8 => ast::DataType::DoublePrecision,
        BaseType::Numeric if data_type.typmod == PgType::NO_TYPEMOD => {
            ast::DataType::Numeric(ast::ExactNumberInfo::None)
        }
        BaseType::Numeric => {
            let encoded = data_type.typmod - 4;
            ast::DataType::Numeric(ast::ExactNumberInfo::PrecisionAndScale(
                u64::try_from(encoded >> 16).expect("valid numeric precision"),
                i64::from(encoded & 0xffff),
            ))
        }
        BaseType::Text => ast::DataType::Text,
        BaseType::Varchar if data_type.typmod == PgType::NO_TYPEMOD => ast::DataType::Varchar(None),
        BaseType::Varchar => ast::DataType::Varchar(Some(ast::CharacterLength::IntegerLength {
            length: u64::try_from(data_type.typmod - 4).expect("valid character typmod"),
            unit: None,
        })),
        BaseType::Bpchar if data_type.typmod == PgType::NO_TYPEMOD => {
            ast::DataType::Custom(ast::Ident::new("bpchar").into(), Vec::new())
        }
        BaseType::Bpchar => ast::DataType::Char(Some(ast::CharacterLength::IntegerLength {
            length: u64::try_from(data_type.typmod - 4).expect("valid character typmod"),
            unit: None,
        })),
        BaseType::Bytea => ast::DataType::Bytea,
        BaseType::Uuid => ast::DataType::Uuid,
        BaseType::Date => ast::DataType::Date,
        BaseType::Time => ast::DataType::Time(
            (data_type.typmod != PgType::NO_TYPEMOD)
                .then(|| u64::try_from(data_type.typmod).expect("valid time precision")),
            ast::TimezoneInfo::WithoutTimeZone,
        ),
        BaseType::Timestamp => ast::DataType::Timestamp(
            (data_type.typmod != PgType::NO_TYPEMOD)
                .then(|| u64::try_from(data_type.typmod).expect("valid timestamp precision")),
            ast::TimezoneInfo::WithoutTimeZone,
        ),
        BaseType::TimestampTz => ast::DataType::Timestamp(
            (data_type.typmod != PgType::NO_TYPEMOD)
                .then(|| u64::try_from(data_type.typmod).expect("valid timestamptz precision")),
            ast::TimezoneInfo::WithTimeZone,
        ),
        BaseType::Interval => ast::DataType::Interval {
            fields: None,
            precision: None,
        },
    }
}
