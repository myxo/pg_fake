use crate::{
    catalog::{Catalog, TableSchema},
    error::{Result, reject_unsupported},
    executor,
};
use sqlparser::ast;

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
pub(super) fn bind_update_scope(
    update: &ast::Update,
    catalog: &Catalog,
) -> Result<executor::BoundScope> {
    let schema = resolve_table_schema(&update.table.relation, catalog)?;
    Ok(executor::combine_bound_scopes(
        executor::bind_target_scope(schema, get_table_alias(&update.table.relation)),
        executor::bind_from_scope(catalog, get_update_from(update)?)?,
    ))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn bind_delete_scope(
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
pub(super) fn resolve_table_schema<'a>(
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
pub(super) fn is_projection_alias(expression: &ast::Expr, projection: &[ast::SelectItem]) -> bool {
    let ast::Expr::Identifier(identifier) = expression else {
        return false;
    };
    projection.iter().any(|item| {
        matches!(item, ast::SelectItem::ExprWithAlias { alias, .. }
            if executor::normalize_identifier(alias) == executor::normalize_identifier(identifier))
    })
}
