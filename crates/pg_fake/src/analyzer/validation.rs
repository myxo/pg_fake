use super::scopes::{
    bind_delete_scope, bind_update_scope, is_projection_alias, resolve_table_schema,
};
use crate::{
    catalog::Catalog,
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState},
    executor,
    value::{BaseType, PgType},
};
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn validate_statement(statement: &ast::Statement, catalog: &Catalog) -> Result<()> {
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
