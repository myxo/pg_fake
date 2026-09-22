use super::scopes::{bind_delete_scope, bind_update_scope, resolve_table_schema};
use crate::{
    catalog::Catalog,
    error::{PgError, Result, SqlState},
    executor,
    value::BaseType,
};
use sqlparser::ast;

mod expressions;
mod queries;

use expressions::infer_expression_parameters;
use queries::infer_query_parameters;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn constrain_statement_parameters(
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
                let (name, subscript) = match &assignment.target {
                    ast::AssignmentTarget::ColumnName(name) => (name, None),
                    ast::AssignmentTarget::Subscript { column, subscripts } => {
                        let [ast::Subscript::Index { index }] = subscripts.as_slice() else {
                            continue;
                        };
                        (column, Some(index))
                    }
                    ast::AssignmentTarget::Tuple(_) => continue,
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
                    Some(
                        subscript
                            .and_then(|_| column.data_type.base.get_array_element_type())
                            .unwrap_or(column.data_type.base),
                    ),
                    types,
                )?;
                if let Some(subscript) = subscript {
                    infer_expression_parameters(subscript, scope, Some(BaseType::Int4), types)?;
                }
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
pub(super) fn finalize_parameter_types(types: Vec<Option<BaseType>>) -> Vec<BaseType> {
    types
        .into_iter()
        .map(|data_type| data_type.unwrap_or(BaseType::Text))
        .collect()
}
