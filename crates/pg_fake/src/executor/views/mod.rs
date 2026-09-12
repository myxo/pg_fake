use crate::executor::{
    DatabaseState, expand_ctes_for_analysis, normalize_identifier, normalize_relation_name,
    scope::infer_query_output_columns,
};
use crate::{
    StatementResult,
    catalog::{RelationName, TEMP_SCHEMA, ViewColumn, ViewDependency, ViewSchema},
    error::{PgError, Result, SqlState, reject_unsupported},
};
use sqlparser::ast;
use std::collections::BTreeSet;

mod binding;
mod column_dependencies;
mod cte_scope;
mod expansion;
mod references;

use binding::{bind_view_dependencies, has_view_dependency_path};
use column_dependencies::collect_view_column_dependencies;
pub(crate) use column_dependencies::has_view_column_dependency;
pub(crate) use expansion::expand_query_views;
pub(crate) use references::{
    preserve_column_drop_references, rename_column_references, rename_table_references,
};

fn quote_identifier(identifier: &str) -> String {
    ast::Ident::with_quote('"', identifier).to_string()
}

pub(crate) fn execute_create_view(
    state: &mut DatabaseState,
    create: &ast::CreateView,
) -> Result<StatementResult> {
    if create.or_alter
        || create.materialized
        || create.secure
        || create.if_not_exists
        || create.name_before_not_exists
        || !matches!(create.options, ast::CreateTableOptions::None)
        || !create.cluster_by.is_empty()
        || create.comment.is_some()
        || create.with_no_schema_binding
        || create.copy_grants
        || create.to.is_some()
        || create.params.is_some()
    {
        return reject_unsupported("CREATE VIEW variant is not implemented");
    }
    if create
        .columns
        .iter()
        .any(|column| column.data_type.is_some() || column.options.is_some())
    {
        return reject_unsupported("CREATE VIEW column options are not implemented");
    }
    if crate::analyzer::count_parameters(&ast::Statement::Query(create.query.clone()))? != 0 {
        return Err(PgError::create(
            SqlState::UndefinedParameter,
            "there is no parameter in CREATE VIEW",
        ));
    }
    let name = normalize_relation_name(&create.name)?;
    let temporary = create.temporary || name.schema.as_deref() == Some(TEMP_SCHEMA);
    let resolved = state.catalog.resolve_creation_name(&name, temporary)?;
    let resolved_name = RelationName::create(
        Some(state.catalog.get_schema_name(resolved.schema_id).to_owned()),
        resolved.name.clone(),
    );
    let existing = state
        .catalog
        .require_named_view(&resolved_name)
        .ok()
        .cloned();
    if !create.or_replace && state.catalog.has_resolved_relation(&resolved) {
        return Err(PgError::create(
            SqlState::DuplicateTable,
            format!("relation {:?} already exists", resolved.name),
        ));
    }
    if create.or_replace && existing.is_none() && state.catalog.has_resolved_relation(&resolved) {
        return Err(PgError::create(
            SqlState::WrongObjectType,
            format!("{:?} is not a view", resolved.name),
        ));
    }
    let statement = ast::Statement::Query(create.query.clone());
    let (expanded, mutations) = expand_ctes_for_analysis(&statement, state)?;
    if !mutations.is_empty() {
        return Err(PgError::create(
            SqlState::FeatureNotSupported,
            "views cannot contain data-modifying statements",
        ));
    }
    let ast::Statement::Query(expanded) = expanded.as_ref() else {
        unreachable!("view definition is a query")
    };
    let inferred = infer_query_output_columns(&state.catalog, expanded)?;
    if create.columns.len() > inferred.len() {
        return Err(PgError::create(
            SqlState::InvalidTableDefinition,
            "CREATE VIEW specifies more column names than columns",
        ));
    }
    let columns = inferred
        .into_iter()
        .enumerate()
        .map(|(index, (name, data_type))| ViewColumn {
            name: create
                .columns
                .get(index)
                .map(|column| normalize_identifier(&column.name))
                .unwrap_or(name),
            data_type,
        })
        .collect::<Vec<_>>();
    let mut names = BTreeSet::new();
    if columns
        .iter()
        .any(|column| !names.insert(column.name.clone()))
    {
        return Err(PgError::create(
            SqlState::DuplicateColumn,
            "column name specified more than once",
        ));
    }
    let (query, dependencies) = bind_view_dependencies(state, &create.query, !temporary)?;
    let column_dependencies =
        collect_view_column_dependencies(&state.catalog, &query, &dependencies);
    if let Some(existing) = existing {
        if columns.len() < existing.columns.len()
            || existing
                .columns
                .iter()
                .zip(&columns)
                .any(|(old, new)| old.name != new.name || old.data_type != new.data_type)
        {
            return Err(PgError::create(
                SqlState::InvalidTableDefinition,
                "cannot change name or data type of view column",
            ));
        }
        if dependencies.iter().any(|dependency| {
            matches!(dependency, ViewDependency::View(id) if has_view_dependency_path(&state.catalog, *id, existing.id))
        }) {
            return Err(PgError::create(
                SqlState::InvalidObjectDefinition,
                "infinite recursion detected in rules for relation",
            ));
        }
        state.catalog.replace_view(ViewSchema {
            id: existing.id,
            schema_id: existing.schema_id,
            name: existing.name,
            columns,
            query,
            comment: existing.comment,
            dependencies,
            column_dependencies,
        })?;
    } else {
        state.catalog.create_named_view(
            resolved,
            columns,
            query,
            dependencies,
            column_dependencies,
        )?;
    }
    Ok(StatementResult::Affected(0))
}

pub(crate) fn execute_drop_views(
    state: &mut DatabaseState,
    names: &[ast::ObjectName],
    if_exists: bool,
    cascade: bool,
) -> Result<StatementResult> {
    if cascade {
        return reject_unsupported("DROP VIEW CASCADE is not implemented");
    }
    let mut views = Vec::new();
    let mut seen = BTreeSet::new();
    for object in names {
        let name = normalize_relation_name(object)?;
        match state.catalog.require_named_view(&name) {
            Ok(view) if seen.insert(view.id) => views.push(name),
            Ok(_) => {}
            Err(error) if if_exists && error.sqlstate == SqlState::UndefinedTable => {}
            Err(error) => return Err(error),
        }
    }
    state.catalog.drop_named_views(&views)?;
    Ok(StatementResult::Affected(0))
}

pub(crate) fn execute_comment_on_view(
    state: &mut DatabaseState,
    name: &ast::ObjectName,
    comment: &Option<String>,
) -> Result<StatementResult> {
    let name = normalize_relation_name(name)?;
    let mut view = state.catalog.require_named_view(&name)?.clone();
    view.comment = comment.clone();
    state.catalog.replace_view(view)?;
    Ok(StatementResult::Affected(0))
}
