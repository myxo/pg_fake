use super::{
    BoundColumn, BoundScope, bind_target_scope, joins::bind_table_with_joins,
    output::describe_bound_query_columns, subqueries::infer_expression_data_type,
};
use crate::executor::{
    DatabaseState,
    expressions::{extract_unknown_string_literal, is_null_literal},
    json, normalize_identifier, normalize_relation_name,
};
use crate::{
    catalog::{Catalog, TableSchema, ViewSchema},
    error::{PgError, Result, SqlState, reject_unsupported},
    value::PgType,
};
use sqlparser::ast;
use std::ops::ControlFlow;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn bind_query_scope(catalog: &Catalog, select: &ast::Select) -> Result<BoundScope> {
    bind_from_scope(catalog, &select.from)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn bind_from_scope(
    catalog: &Catalog,
    from: &[ast::TableWithJoins],
) -> Result<BoundScope> {
    let mut scope = BoundScope {
        columns: Vec::new(),
    };
    for source in from {
        bind_table_with_joins(catalog, source, &mut scope)?;
    }
    Ok(scope)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn bind_query_scope_with_outer(
    catalog: &Catalog,
    select: &ast::Select,
    outer: &BoundScope,
) -> Result<BoundScope> {
    let mut scope = bind_query_scope(catalog, select)?;
    let start = scope.columns.len();
    scope.columns.extend(outer.columns.iter().map(|column| {
        let mut column = column.clone();
        column.slot += start;
        for slots in [&mut column.merged, &mut column.qualified_merged]
            .into_iter()
            .flatten()
        {
            for slot in slots {
                *slot += start;
            }
        }
        column.output_order += start;
        column.qualified_order += start;
        column.depth += 1;
        column.unqualified = true;
        column.wildcard = false;
        column
    }));
    Ok(scope)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn bind_table_factor(
    catalog: &Catalog,
    factor: &ast::TableFactor,
    scope: &mut BoundScope,
) -> Result<()> {
    if let Some(json::JsonTableFunction {
        name,
        argument,
        alias,
        ordinality,
    }) = json::extract_json_table_function(factor)?
    {
        let base = json::resolve_json_function_arguments(&name).expect("JSON expansion")[0];
        let mut parameterized = false;
        let _ = ast::visit_expressions(argument, |expr| {
            if matches!(expr, ast::Expr::Value(value) if matches!(value.value, ast::Value::Placeholder(_)))
            {
                parameterized = true;
            }
            ControlFlow::<()>::Continue(())
        });
        if !parameterized
            && !is_null_literal(argument)
            && extract_unknown_string_literal(argument).is_none()
        {
            let source = infer_expression_data_type(catalog, argument, scope)?.base;
            if !crate::coercion::can_cast(source, base, crate::coercion::CastContext::Implicit) {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    "JSON table function argument has incompatible type",
                ));
            }
        }
        let columns = json::describe_json_expansion(&name, ordinality);
        if alias.is_some_and(|alias| alias.columns.len() > columns.len()) {
            return Err(PgError::create(
                SqlState::InvalidColumnReference,
                "function has fewer columns than its alias list",
            ));
        }
        let qualifier = alias
            .map(|a| normalize_identifier(&a.name))
            .unwrap_or_else(|| name.clone());
        let start = scope.columns.len();
        for (index, (mut column_name, base)) in columns.into_iter().enumerate() {
            if index == 0
                && name.ends_with("object_keys")
                && let Some(alias) = alias
            {
                column_name = normalize_identifier(&alias.name);
            }
            let source_name = column_name.clone();
            if let Some(alias) = alias.and_then(|a| a.columns.get(index)) {
                column_name = normalize_identifier(&alias.name);
            }
            scope.columns.push(BoundColumn {
                name: column_name,
                data_type: PgType::create(base),
                qualifier: qualifier.clone(),
                slot: start + index,
                output_order: start + index,
                qualified_order: start + index,
                qualified_merged: None,
                merged: None,
                unqualified: true,
                wildcard: true,
                depth: 0,
                table_id: None,
                source_name,
            });
        }
        return Ok(());
    }
    if let ast::TableFactor::NestedJoin {
        table_with_joins,
        alias,
    } = factor
    {
        let start = scope.columns.len();
        bind_table_with_joins(catalog, table_with_joins, scope)?;
        if let Some(alias) = alias {
            let output_count = scope.columns[start..]
                .iter()
                .filter(|column| column.wildcard)
                .count();
            if alias.columns.len() > output_count {
                return Err(PgError::create(
                    SqlState::InvalidColumnReference,
                    "join has fewer columns than specified in the column alias list",
                ));
            }
            let qualifier = normalize_identifier(&alias.name);
            let mut order = (start..scope.columns.len()).collect::<Vec<_>>();
            order.sort_by_key(|i| scope.columns[*i].output_order);
            let mut output_index = 0;
            for index in order {
                let column = &mut scope.columns[index];
                if column.wildcard {
                    if let Some(alias) = alias.columns.get(output_index) {
                        column.name = normalize_identifier(&alias.name);
                    }
                    column.qualifier = qualifier.clone();
                    column.qualified_order = output_index;
                    column.qualified_merged = column.merged.clone();
                    output_index += 1;
                } else {
                    column.qualifier.clear();
                }
            }
        }
        return Ok(());
    }
    if let ast::TableFactor::Derived {
        lateral,
        subquery,
        alias,
        ..
    } = factor
    {
        let bound;
        let subquery = if *lateral {
            (bound, _) = crate::executor::bind_lateral_query(
                catalog,
                subquery,
                scope,
                &vec![crate::value::Value::Null; scope.columns.len()],
            )?;
            &bound
        } else {
            subquery.as_ref()
        };
        let subquery = crate::analyzer::bind_query_parameters_for_analysis(subquery, catalog)?;
        let subquery = crate::executor::ctes::inline_query_ctes(&subquery, catalog, None, true)?;
        let columns = describe_bound_query_columns(catalog, &subquery, None)?;
        if alias
            .as_ref()
            .is_some_and(|alias| alias.columns.len() > columns.len())
        {
            return Err(PgError::create(
                SqlState::InvalidColumnReference,
                "derived table has fewer columns than specified in the column alias list",
            ));
        }
        let qualifier = alias
            .as_ref()
            .map(|alias| normalize_identifier(&alias.name))
            .unwrap_or_default();
        let start = scope.columns.len();
        scope
            .columns
            .extend(columns.into_iter().enumerate().map(|(index, column)| {
                let source_name = column.name.clone();
                BoundColumn {
                    name: alias
                        .as_ref()
                        .and_then(|alias| alias.columns.get(index))
                        .map(|alias| normalize_identifier(&alias.name))
                        .unwrap_or(column.name),
                    data_type: column.data_type,
                    qualifier: qualifier.clone(),
                    slot: start + index,
                    output_order: start + index,
                    qualified_order: start + index,
                    qualified_merged: None,
                    merged: None,
                    unqualified: true,
                    wildcard: true,
                    depth: 0,
                    table_id: None,
                    source_name,
                }
            }));
        return Ok(());
    }
    let ast::TableFactor::Table {
        name: table_name,
        alias,
        args,
        ..
    } = factor
    else {
        return reject_unsupported("FROM source is not implemented");
    };
    if args.is_some() {
        return reject_unsupported("table functions are not implemented");
    }
    let name = normalize_relation_name(table_name)?;
    if let Some(columns) = crate::executor::describe_visible_system_relation(catalog, &name) {
        if alias
            .as_ref()
            .is_some_and(|alias| alias.columns.len() > columns.len())
        {
            return Err(PgError::create(
                SqlState::InvalidColumnReference,
                "relation has fewer columns than specified in its alias list",
            ));
        }
        let qualifier = alias
            .as_ref()
            .map(|alias| normalize_identifier(&alias.name))
            .unwrap_or_else(|| name.name.clone());
        let start = scope.columns.len();
        scope
            .columns
            .extend(columns.iter().enumerate().map(|(index, column)| {
                let source_name = column.name.to_owned();
                BoundColumn {
                    name: alias
                        .as_ref()
                        .and_then(|alias| alias.columns.get(index))
                        .map(|alias| normalize_identifier(&alias.name))
                        .unwrap_or_else(|| source_name.clone()),
                    data_type: column.data_type,
                    qualifier: qualifier.clone(),
                    slot: start + index,
                    output_order: start + index,
                    qualified_order: start + index,
                    qualified_merged: None,
                    merged: None,
                    unqualified: true,
                    wildcard: true,
                    depth: 0,
                    table_id: None,
                    source_name,
                }
            }));
        return Ok(());
    }
    let relation = match catalog.require_named_table(&name) {
        Ok(table) => BoundScope::bind_table(table, alias.as_ref(), scope.columns.len())?,
        Err(error) if error.sqlstate == SqlState::WrongObjectType => BoundScope::bind_view(
            catalog.require_named_view(&name)?,
            alias.as_ref(),
            scope.columns.len(),
        )?,
        Err(error) => return Err(error),
    };
    scope.columns.extend(relation.columns);
    Ok(())
}

pub(crate) fn bind_table_factor_scope(
    catalog: &Catalog,
    factor: &ast::TableFactor,
) -> Result<BoundScope> {
    let mut scope = BoundScope {
        columns: Vec::new(),
    };
    bind_table_factor(catalog, factor, &mut scope)?;
    Ok(scope)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn bind_select_scope(
    state: &DatabaseState,
    select: &ast::Select,
) -> Result<BoundScope> {
    bind_query_scope(&state.catalog, select)
}

impl BoundScope {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn bind_table(
        schema: &TableSchema,
        alias: Option<&ast::TableAlias>,
        slot: usize,
    ) -> Result<Self> {
        if alias.is_some_and(|alias| alias.columns.len() > schema.columns.len()) {
            return Err(PgError::create(
                SqlState::InvalidColumnReference,
                "table has fewer columns than specified in the column alias list",
            ));
        }
        let mut scope = bind_target_scope(schema, alias.map(|alias| &alias.name));
        for (index, column) in scope.columns.iter_mut().enumerate() {
            column.slot += slot;
            column.output_order += slot;
            column.qualified_order += slot;
            if let Some(alias) = alias.and_then(|alias| alias.columns.get(index)) {
                column.name = normalize_identifier(&alias.name);
            }
        }
        Ok(scope)
    }

    fn bind_view(view: &ViewSchema, alias: Option<&ast::TableAlias>, slot: usize) -> Result<Self> {
        if alias.is_some_and(|alias| alias.columns.len() > view.columns.len()) {
            return Err(PgError::create(
                SqlState::InvalidColumnReference,
                "view has fewer columns than specified in the column alias list",
            ));
        }
        let qualifier = alias
            .map(|alias| normalize_identifier(&alias.name))
            .unwrap_or_else(|| view.name.clone());
        Ok(BoundScope {
            columns: view
                .columns
                .iter()
                .enumerate()
                .map(|(index, column)| BoundColumn {
                    name: alias
                        .and_then(|alias| alias.columns.get(index))
                        .map(|alias| normalize_identifier(&alias.name))
                        .unwrap_or_else(|| column.name.clone()),
                    data_type: column.data_type,
                    qualifier: qualifier.clone(),
                    slot: slot + index,
                    output_order: slot + index,
                    qualified_order: slot + index,
                    qualified_merged: None,
                    merged: None,
                    unqualified: true,
                    wildcard: true,
                    depth: 0,
                    table_id: None,
                    source_name: column.name.clone(),
                })
                .collect(),
        })
    }
}
