use super::{
    expressions::{
        contains_volatile_expression, evaluate_select_expression, infer_query_expression_type,
    },
    grouping::{AggregateOwner, GroupedAggregateValues},
    set_operations::describe_set_expression_columns,
    values::bind_values_scope,
};
use crate::{
    ColumnMeta,
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{
        DatabaseState, StatementContext, normalize_function_name, normalize_identifier,
        normalize_relation_name, normalize_unqualified_object_name, resolve_insert_table_name,
        scope::{
            BoundScope, bind_from_scope, bind_select_scope, bind_target_scope, combine_bound_scopes,
        },
    },
    txn::{Snapshot, Xid},
    value::{PgType, Value},
};
use sqlparser::ast;

pub(in crate::executor) enum ProjectionSource<'a> {
    Column(usize),
    Merged(Vec<usize>, PgType, Option<String>),
    Expression(&'a ast::Expr),
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn describe_query_result_columns(
    state: &DatabaseState,
    statement: &ast::Statement,
) -> Result<Vec<ColumnMeta>> {
    match statement {
        ast::Statement::Query(query) => match query.body.as_ref() {
            ast::SetExpr::Select(select) => {
                let mut select = select.as_ref().clone();
                super::windows::resolve_select_windows(&mut select)?;
                bind_select_scope(state, &select).and_then(|scope| {
                    build_projection_plan(state, &select.projection, &scope)
                        .map(|(_, columns)| columns)
                })
            }
            ast::SetExpr::Values(values) => bind_values_scope(values).map(|scope| {
                scope
                    .columns
                    .iter()
                    .map(|column| ColumnMeta {
                        name: column.name.clone(),
                        type_oid: column.data_type.map_to_oid(),
                        typmod: column.data_type.typmod,
                    })
                    .collect()
            }),
            _ => describe_set_expression_columns(state, query, &query.body),
        },
        ast::Statement::Insert(insert) => {
            let Some(returning) = &insert.returning else {
                return Ok(Vec::new());
            };
            let schema = state
                .catalog
                .require_named_table(&resolve_insert_table_name(&insert.table)?)?;
            let scope = bind_target_scope(
                schema,
                insert.table_alias.as_ref().map(|alias| &alias.alias),
            );
            build_mutation_projection_plan(state, returning, &scope, schema.columns.len())
                .map(|(_, columns)| columns)
        }
        ast::Statement::Update(update) => {
            let Some(returning) = &update.returning else {
                return Ok(Vec::new());
            };
            let ast::TableFactor::Table {
                name, alias, args, ..
            } = &update.table.relation
            else {
                return Ok(Vec::new());
            };
            if args.is_some() {
                return Ok(Vec::new());
            }
            let schema = state
                .catalog
                .require_named_table(&normalize_relation_name(name)?)?;
            let from = match &update.from {
                None => &[][..],
                Some(ast::UpdateTableFromKind::AfterSet(from)) => from.as_slice(),
                Some(ast::UpdateTableFromKind::BeforeSet(_)) => return Ok(Vec::new()),
            };
            let scope = combine_bound_scopes(
                bind_target_scope(schema, alias.as_ref().map(|alias| &alias.name)),
                bind_from_scope(&state.catalog, from)?,
            );
            build_mutation_projection_plan(state, returning, &scope, schema.columns.len())
                .map(|(_, columns)| columns)
        }
        ast::Statement::Delete(delete) => {
            let Some(returning) = &delete.returning else {
                return Ok(Vec::new());
            };
            let ast::FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(Vec::new());
            };
            let Some(ast::TableWithJoins {
                relation:
                    ast::TableFactor::Table {
                        name, alias, args, ..
                    },
                ..
            }) = from.first()
            else {
                return Ok(Vec::new());
            };
            if args.is_some() {
                return Ok(Vec::new());
            }
            let schema = state
                .catalog
                .require_named_table(&normalize_relation_name(name)?)?;
            let scope = combine_bound_scopes(
                bind_target_scope(schema, alias.as_ref().map(|alias| &alias.name)),
                bind_from_scope(&state.catalog, delete.using.as_deref().unwrap_or_default())?,
            );
            build_mutation_projection_plan(state, returning, &scope, schema.columns.len())
                .map(|(_, columns)| columns)
        }
        _ => Ok(Vec::new()),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn create_projection_expression(
    projection: &ProjectionSource<'_>,
    scope: &BoundScope,
) -> ast::Expr {
    match projection {
        ProjectionSource::Expression(expression) => (*expression).clone(),
        ProjectionSource::Column(slot) => {
            let column = scope
                .columns
                .iter()
                .find(|column| column.slot == *slot)
                .expect("projected column is present in the bound scope");
            ast::Expr::CompoundIdentifier(vec![
                ast::Ident::new(column.qualifier.clone()),
                ast::Ident::new(column.name.clone()),
            ])
        }
        ProjectionSource::Merged(slots, _, qualifier) => {
            let column = scope
                .columns
                .iter()
                .find(|column| match qualifier {
                    Some(qualifier) => {
                        column.qualifier == *qualifier
                            && column.qualified_merged.as_ref() == Some(slots)
                    }
                    None => column.merged.as_ref() == Some(slots) && column.wildcard,
                })
                .expect("projected merged column is present in the bound scope");
            match qualifier {
                Some(qualifier) => ast::Expr::CompoundIdentifier(vec![
                    ast::Ident::new(qualifier.clone()),
                    ast::Ident::new(column.name.clone()),
                ]),
                None => ast::Expr::Identifier(ast::Ident::new(column.name.clone())),
            }
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_projection_value(
    state: &DatabaseState,
    projection: &ProjectionSource<'_>,
    scope: &BoundScope,
    row: &[Value],
    aggregate_values: Option<(&GroupedAggregateValues, AggregateOwner)>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Value> {
    match projection {
        ProjectionSource::Column(index) => Ok(row[*index].clone()),
        ProjectionSource::Merged(slots, data_type, _) => {
            let value = slots
                .iter()
                .map(|slot| &row[*slot])
                .find(|value| !value.is_null())
                .cloned()
                .unwrap_or(Value::Null);
            if value.is_null() {
                Ok(value)
            } else {
                coercion::coerce(
                    value.clone(),
                    value
                        .get_base_type()
                        .expect("non-null value has a base type"),
                    *data_type,
                    CastContext::Implicit,
                    &context.get_timezone(),
                )
            }
        }
        ProjectionSource::Expression(expression) => evaluate_select_expression(
            state,
            expression,
            scope,
            row,
            aggregate_values,
            xid,
            snapshot,
            context,
        ),
    }
}

pub(super) fn contains_volatile_projection(projection: &ProjectionSource<'_>) -> bool {
    matches!(projection, ProjectionSource::Expression(expression) if contains_volatile_expression(expression))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn evaluate_projection_values(
    state: &DatabaseState,
    projections: &[ProjectionSource<'_>],
    scope: &BoundScope,
    row: &[Value],
    aggregate_values: Option<&GroupedAggregateValues>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Vec<Value>> {
    projections
        .iter()
        .enumerate()
        .map(|(index, projection)| {
            evaluate_projection_value(
                state,
                projection,
                scope,
                row,
                aggregate_values.map(|values| (values, AggregateOwner::Projection(index))),
                xid,
                snapshot,
                context,
            )
        })
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn build_projection_plan<'a>(
    state: &DatabaseState,
    projection: &'a [ast::SelectItem],
    scope: &BoundScope,
) -> Result<(Vec<ProjectionSource<'a>>, Vec<ColumnMeta>)> {
    let mut projections = Vec::new();
    let mut columns = Vec::new();
    for item in projection {
        match item {
            ast::SelectItem::Wildcard(_) => {
                for column in scope.select_wildcard_columns(None) {
                    if column.wildcard {
                        projections.push(match &column.merged {
                            Some(slots) => {
                                ProjectionSource::Merged(slots.clone(), column.data_type, None)
                            }
                            None => ProjectionSource::Column(column.slot),
                        });
                        columns.push(ColumnMeta {
                            name: column.name.clone(),
                            type_oid: column.data_type.map_to_oid(),
                            typmod: column.data_type.typmod,
                        });
                    }
                }
            }
            ast::SelectItem::QualifiedWildcard(
                ast::SelectItemQualifiedWildcardKind::ObjectName(object_name),
                _,
            ) => {
                let qualifier = normalize_unqualified_object_name(object_name)?;
                let matching = scope.select_wildcard_columns(Some(&qualifier));
                if matching.is_empty()
                    && !scope
                        .columns
                        .iter()
                        .any(|column| column.qualifier == qualifier)
                {
                    return Err(PgError::create(
                        SqlState::UndefinedTable,
                        format!("missing FROM-clause entry for table {qualifier:?}"),
                    ));
                }
                for column in matching {
                    projections.push(if let Some(slots) = &column.qualified_merged {
                        ProjectionSource::Merged(
                            slots.clone(),
                            column.data_type,
                            Some(column.qualifier.clone()),
                        )
                    } else {
                        ProjectionSource::Column(column.slot)
                    });
                    columns.push(ColumnMeta {
                        name: column.name.clone(),
                        type_oid: column.data_type.map_to_oid(),
                        typmod: column.data_type.typmod,
                    });
                }
            }
            ast::SelectItem::UnnamedExpr(expression @ ast::Expr::Identifier(column)) => {
                let (_, data_type) = scope.resolve_column(std::slice::from_ref(column))?;
                projections.push(ProjectionSource::Expression(expression));
                columns.push(ColumnMeta {
                    name: column.value.clone(),
                    type_oid: data_type.map_to_oid(),
                    typmod: data_type.typmod,
                });
            }
            ast::SelectItem::UnnamedExpr(
                expression @ ast::Expr::CompoundIdentifier(identifiers),
            ) => {
                let (slot, data_type) = scope.resolve_column(identifiers)?;
                if scope.columns.iter().any(|column| {
                    column.slot == slot && column.wildcard && column.qualified_merged.is_none()
                }) {
                    projections.push(ProjectionSource::Column(slot));
                } else {
                    projections.push(ProjectionSource::Expression(expression));
                }
                columns.push(ColumnMeta {
                    name: identifiers
                        .last()
                        .expect("compound identifier is non-empty")
                        .value
                        .clone(),
                    type_oid: data_type.map_to_oid(),
                    typmod: data_type.typmod,
                });
            }
            ast::SelectItem::UnnamedExpr(expr) => {
                let data_type = infer_query_expression_type(state, expr, scope)?;
                projections.push(ProjectionSource::Expression(expr));
                columns.push(ColumnMeta {
                    name: match expr {
                        ast::Expr::Function(function) => normalize_function_name(&function.name)?,
                        ast::Expr::Extract { .. } => "extract".into(),
                        ast::Expr::Floor { .. } => "floor".into(),
                        ast::Expr::AtTimeZone { .. } => "timezone".into(),
                        _ => "?column?".into(),
                    },
                    type_oid: data_type.map_to_oid(),
                    typmod: data_type.typmod,
                });
            }
            ast::SelectItem::ExprWithAlias { expr, alias } => {
                let resolved = match expr {
                    ast::Expr::Identifier(column) => {
                        Some(scope.resolve_column(std::slice::from_ref(column))?)
                    }
                    ast::Expr::CompoundIdentifier(identifiers) => {
                        Some(scope.resolve_column(identifiers)?)
                    }
                    _ => None,
                };
                let (projection, data_type, typmod) = match resolved {
                    Some((_, data_type)) => (
                        ProjectionSource::Expression(expr),
                        data_type,
                        data_type.typmod,
                    ),
                    None => {
                        let data_type = infer_query_expression_type(state, expr, scope)?;
                        (
                            ProjectionSource::Expression(expr),
                            data_type,
                            data_type.typmod,
                        )
                    }
                };
                projections.push(projection);
                columns.push(ColumnMeta {
                    name: normalize_identifier(alias),
                    type_oid: data_type.map_to_oid(),
                    typmod,
                });
            }
            _ => {
                return reject_unsupported("SELECT projection is not implemented");
            }
        }
    }
    Ok((projections, columns))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn build_mutation_projection_plan<'a>(
    state: &DatabaseState,
    projection: &'a [ast::SelectItem],
    scope: &BoundScope,
    target_columns: usize,
) -> Result<(Vec<ProjectionSource<'a>>, Vec<ColumnMeta>)> {
    let mut target_wildcard_scope = scope.clone();
    for column in &mut target_wildcard_scope.columns[target_columns..] {
        column.wildcard = false;
    }
    let mut projections = Vec::new();
    let mut columns = Vec::new();
    for item in projection {
        let item_scope = if matches!(item, ast::SelectItem::Wildcard(_)) {
            &target_wildcard_scope
        } else {
            scope
        };
        let (mut item_projections, mut item_columns) =
            build_projection_plan(state, std::slice::from_ref(item), item_scope)?;
        projections.append(&mut item_projections);
        columns.append(&mut item_columns);
    }
    Ok((projections, columns))
}
