use super::*;
use sqlparser::ast;

struct ReturningPlan<'a> {
    scope: BoundScope,
    projections: Vec<query::ProjectionSource<'a>>,
    columns: Vec<ColumnMeta>,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn build_returning_plan<'a>(
    state: &DatabaseState,
    scope: BoundScope,
    target_columns: usize,
    returning: Option<&'a [ast::SelectItem]>,
) -> Result<Option<ReturningPlan<'a>>> {
    let Some(returning) = returning else {
        return Ok(None);
    };
    let (projections, columns) =
        query::build_mutation_projection_plan(state, returning, &scope, target_columns)?;
    Ok(Some(ReturningPlan {
        scope,
        projections,
        columns,
    }))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_mutation_scope(
    state: &DatabaseState,
    schema: &TableSchema,
    alias: Option<&ast::Ident>,
    from: &[ast::TableWithJoins],
) -> Result<BoundScope> {
    Ok(combine_bound_scopes(
        bind_target_scope(schema, alias),
        bind_from_scope(&state.catalog, from)?,
    ))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materialize_mutation_source_rows(
    state: &DatabaseState,
    from: &[ast::TableWithJoins],
    scope: &BoundScope,
    target_columns: usize,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<Vec<Value>>> {
    if from.is_empty() {
        return Ok(vec![vec![Value::Null; scope.columns.len()]]);
    }
    query::materialize_from_rows(
        state,
        from,
        scope,
        target_columns,
        xid,
        snapshot,
        context,
        None,
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_mutation_assignment(
    state: &DatabaseState,
    expression: &ast::Expr,
    target: PgType,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Value> {
    if let Some(text) = extract_unknown_string_literal(expression) {
        coercion::coerce_unknown(text, target, CastContext::Assignment)
    } else {
        coercion::coerce(
            query::evaluate_query_expression(
                state, expression, scope, row, xid, snapshot, context,
            )?,
            query::infer_query_expression_type(state, expression, scope)?.base,
            target,
            CastContext::Assignment,
        )
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn matches_mutation_row(
    state: &DatabaseState,
    selection: Option<&ast::Expr>,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<bool> {
    let Some(selection) = selection else {
        return Ok(true);
    };
    Ok(
        match query::evaluate_query_expression(
            state, selection, scope, row, xid, snapshot, context,
        )? {
            Value::Bool(value) => value,
            Value::Null => false,
            _ => unreachable!("WHERE expression was type-checked"),
        },
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_returning_row(
    state: &DatabaseState,
    returning: Option<&ReturningPlan<'_>>,
    row: &[Value],
    rows: &mut Vec<Vec<Value>>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<()> {
    let Some(returning) = returning else {
        return Ok(());
    };
    rows.push(query::evaluate_projection_values(
        state,
        &returning.projections,
        &returning.scope,
        row,
        None,
        xid,
        snapshot,
        context,
    )?);
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_write_result(
    affected: u64,
    returning: Option<ReturningPlan<'_>>,
    rows: Vec<Vec<Value>>,
) -> StatementResult {
    match returning {
        Some(returning) => StatementResult::Query(QueryResult {
            columns: returning.columns,
            rows,
        }),
        None => StatementResult::Affected(affected),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_insert(
    state: &mut DatabaseState,
    insert: &ast::Insert,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    let table_name = resolve_insert_table_name(&insert.table)?;
    let schema = state.catalog.require_table(&table_name)?.clone();
    let returning_scope = bind_target_scope(
        &schema,
        insert.table_alias.as_ref().map(|alias| &alias.alias),
    );
    let returning = build_returning_plan(
        state,
        returning_scope,
        schema.columns.len(),
        insert.returning.as_deref(),
    )?;
    let column_indexes = if insert.columns.is_empty() {
        (0..schema.columns.len()).collect::<Vec<_>>()
    } else {
        insert
            .columns
            .iter()
            .map(|name| {
                let name = crate::executor::normalize_unqualified_object_name(name)?;
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
    let provided = column_indexes.iter().copied().collect::<BTreeSet<_>>();
    let build_row = |expressions: &[ast::Expr]| -> Result<Vec<Value>> {
        if expressions.len() != column_indexes.len() {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "INSERT has wrong number of values",
            ));
        }
        let mut row = vec![Value::Null; schema.columns.len()];
        for (index, column) in schema.columns.iter().enumerate() {
            if !provided.contains(&index) {
                row[index] = evaluate_column_default(column, context)?;
            }
        }
        let constants = create_constant_expression_schema();
        for (expr, index) in expressions.iter().zip(&column_indexes) {
            if schema.columns[*index].identity == Some(IdentityKind::Always)
                && !is_default_expression(expr)
            {
                return Err(PgError::create(
                    SqlState::GeneratedAlways,
                    format!(
                        "cannot insert a non-DEFAULT value into column {:?}",
                        schema.columns[*index].name
                    ),
                ));
            }
            row[*index] = if is_default_expression(expr) {
                evaluate_column_default(&schema.columns[*index], context)?
            } else {
                evaluate_assignment_expression(
                    expr,
                    schema.columns[*index].data_type,
                    &constants,
                    &[],
                    context,
                )?
            };
        }
        validate_not_null(&schema, &row)?;
        validate_check_constraints(&schema, &row, context)?;
        Ok(row)
    };
    let rows = if let Some(source) = &insert.source {
        if let ast::SetExpr::Values(values) = source.body.as_ref() {
            values
                .rows
                .iter()
                .map(|expressions| build_row(expressions))
                .collect::<Result<Vec<_>>>()?
        } else {
            if column_indexes
                .iter()
                .any(|index| schema.columns[*index].identity == Some(IdentityKind::Always))
            {
                return Err(PgError::create(
                    SqlState::GeneratedAlways,
                    "cannot insert a non-DEFAULT value into an identity column",
                ));
            }
            let unknown_columns = identify_unknown_query_columns(source, column_indexes.len());
            let StatementResult::Query(source) =
                query::execute_query(state, source, xid, snapshot, context)?
            else {
                unreachable!("query execution returns rows")
            };
            if source.columns.len() != column_indexes.len() {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "INSERT has wrong number of values",
                ));
            }
            source
                .rows
                .into_iter()
                .map(|values| {
                    let mut row = vec![Value::Null; schema.columns.len()];
                    for (index, column) in schema.columns.iter().enumerate() {
                        if !provided.contains(&index) {
                            row[index] = evaluate_column_default(column, context)?;
                        }
                    }
                    for (((value, source_column), unknown), index) in values
                        .into_iter()
                        .zip(&source.columns)
                        .zip(&unknown_columns)
                        .zip(&column_indexes)
                    {
                        row[*index] = if *unknown {
                            let Value::Text(text) = value else {
                                unreachable!("unknown string literals evaluate to text")
                            };
                            coercion::coerce_unknown(
                                &text,
                                schema.columns[*index].data_type,
                                CastContext::Assignment,
                            )?
                        } else {
                            let source_type = BaseType::resolve_oid(source_column.type_oid)
                                .expect("query columns use supported PostgreSQL types");
                            coercion::coerce(
                                value,
                                source_type,
                                schema.columns[*index].data_type,
                                CastContext::Assignment,
                            )?
                        };
                    }
                    validate_not_null(&schema, &row)?;
                    validate_check_constraints(&schema, &row, context)?;
                    Ok(row)
                })
                .collect::<Result<Vec<_>>>()?
        }
    } else {
        assert!(insert.columns.is_empty());
        schema
            .columns
            .iter()
            .map(|column| evaluate_column_default(column, context))
            .collect::<Result<Vec<_>>>()
            .and_then(|row| {
                validate_not_null(&schema, &row)?;
                validate_check_constraints(&schema, &row, context)?;
                Ok(vec![row])
            })?
    };
    let affected = rows.len() as u64;
    let mut returned_rows = Vec::new();
    for row in rows {
        if state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .has_visible_unique_conflict(&row, snapshot, xid, &state.transactions, None)
        {
            return Err(PgError::create(
                SqlState::UniqueViolation,
                format!(
                    "duplicate key value violates unique constraint on {:?}",
                    schema.name
                ),
            ));
        }
        state
            .tables
            .get_mut(&schema.id)
            .expect("catalog table must have storage")
            .insert(xid, context.command_id, row.clone());
        state.mark_table_touched(xid, schema.id);
        validate_row_foreign_keys(
            state,
            &schema,
            &row,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
        )?;
        evaluate_returning_row(
            state,
            returning.as_ref(),
            &row,
            &mut returned_rows,
            xid,
            snapshot,
            context,
        )?;
    }
    Ok(create_write_result(affected, returning, returned_rows))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_update(
    state: &mut DatabaseState,
    update_table: &ast::TableWithJoins,
    assignments: &[ast::Assignment],
    from: Option<&ast::UpdateTableFromKind>,
    selection: Option<&ast::Expr>,
    returning_items: Option<&[ast::SelectItem]>,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    context: &StatementExecutionContext,
    mutation_targets: Option<Vec<RequiredRowLock>>,
) -> Result<StatementResult> {
    if !update_table.joins.is_empty() {
        return reject_unsupported("UPDATE joins are not implemented");
    }
    let ast::TableFactor::Table {
        name: table_name,
        alias,
        args,
        ..
    } = &update_table.relation
    else {
        return reject_unsupported("UPDATE target is not implemented");
    };
    if args.is_some() {
        return reject_unsupported("UPDATE table functions are not implemented");
    }
    let schema = state
        .catalog
        .require_table(&normalize_unqualified_object_name(table_name)?)?
        .clone();
    let from = match from {
        None => &[][..],
        Some(ast::UpdateTableFromKind::AfterSet(from)) => from.as_slice(),
        Some(ast::UpdateTableFromKind::BeforeSet(_)) => {
            return reject_unsupported("UPDATE FROM before SET is not implemented");
        }
    };
    let scope = create_mutation_scope(
        state,
        &schema,
        alias.as_ref().map(|alias| &alias.name),
        from,
    )?;
    let returning =
        build_returning_plan(state, scope.clone(), schema.columns.len(), returning_items)?;
    if let Some(selection) = selection {
        let base = query::infer_query_expression_type(state, selection, &scope)?.base;
        if base != BaseType::Bool && !is_null_literal(selection) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let mut assigned = BTreeSet::new();
    let assignments = assignments
        .iter()
        .map(|assignment| {
            let ast::AssignmentTarget::ColumnName(column) = &assignment.target else {
                return reject_unsupported("UPDATE tuple assignment is not implemented");
            };
            let column_name = normalize_unqualified_object_name(column)?;
            let index = schema
                .columns
                .iter()
                .position(|definition| definition.name == column_name)
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::UndefinedColumn,
                        format!("column {column_name:?} does not exist"),
                    )
                })?;
            if !assigned.insert(index) {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "multiple assignments to the same column",
                ));
            }
            if !is_default_expression(&assignment.value)
                && !is_null_literal(&assignment.value)
                && extract_unknown_string_literal(&assignment.value).is_none()
                && !coercion::can_cast(
                    query::infer_query_expression_type(state, &assignment.value, &scope)?.base,
                    schema.columns[index].data_type.base,
                    CastContext::Assignment,
                )
            {
                return Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "column has incompatible type",
                ));
            }
            Ok((index, &assignment.value))
        })
        .collect::<Result<Vec<_>>>()?;
    let source_rows = materialize_mutation_source_rows(
        state,
        from,
        &scope,
        schema.columns.len(),
        xid,
        snapshot,
        context,
    )?;
    let targets = collect_mutation_targets(
        state,
        &schema,
        selection,
        &scope,
        &source_rows,
        xid,
        snapshot,
        context,
        mutation_targets,
    )?;
    let affected = targets.len() as u64;
    let mut returned_rows = Vec::new();
    for (row_id, version_xmin, row, mut bound_row) in targets {
        let mut updated = row.clone();
        for (index, expression) in &assignments {
            let target = schema.columns[*index].data_type;
            updated[*index] = if is_default_expression(expression) {
                evaluate_column_default(&schema.columns[*index], context)?
            } else {
                evaluate_mutation_assignment(
                    state, expression, target, &scope, &bound_row, xid, snapshot, context,
                )?
            };
        }
        validate_not_null(&schema, &updated)?;
        validate_check_constraints(&schema, &updated, context)?;
        if state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .has_visible_unique_conflict(&updated, snapshot, xid, &state.transactions, Some(row_id))
        {
            return Err(PgError::create(
                SqlState::UniqueViolation,
                format!(
                    "duplicate key value violates unique constraint on {:?}",
                    schema.name
                ),
            ));
        }
        state
            .tables
            .get_mut(&schema.id)
            .expect("catalog table must have storage")
            .append_updated_version(
                row_id,
                version_xmin,
                xid,
                context.command_id,
                updated.clone(),
            );
        state.mark_table_touched(xid, schema.id);
        validate_row_foreign_keys(
            state,
            &schema,
            &updated,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
        )?;
        apply_referencing_foreign_key_actions(
            state,
            &schema,
            &row,
            Some(&updated),
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            &mut BTreeSet::new(),
            context,
        )?;
        bound_row[..schema.columns.len()].clone_from_slice(&updated);
        evaluate_returning_row(
            state,
            returning.as_ref(),
            &bound_row,
            &mut returned_rows,
            xid,
            snapshot,
            context,
        )?;
    }
    Ok(create_write_result(affected, returning, returned_rows))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_delete(
    state: &mut DatabaseState,
    delete: &ast::Delete,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    context: &StatementExecutionContext,
    mutation_targets: Option<Vec<RequiredRowLock>>,
) -> Result<StatementResult> {
    if !delete.tables.is_empty() || !delete.order_by.is_empty() || delete.limit.is_some() {
        return reject_unsupported("DELETE feature is not implemented");
    }
    let ast::FromTable::WithFromKeyword(from) = &delete.from else {
        return reject_unsupported("DELETE without FROM is not implemented");
    };
    if from.len() != 1 || !from[0].joins.is_empty() {
        return reject_unsupported("DELETE joins are not implemented");
    }
    let ast::TableFactor::Table {
        name: table_name,
        alias,
        args,
        ..
    } = &from[0].relation
    else {
        return reject_unsupported("DELETE target is not implemented");
    };
    if args.is_some() {
        return reject_unsupported("DELETE table functions are not implemented");
    }
    let schema = state
        .catalog
        .require_table(&normalize_unqualified_object_name(table_name)?)?
        .clone();
    let using = delete.using.as_deref().unwrap_or_default();
    let scope = create_mutation_scope(
        state,
        &schema,
        alias.as_ref().map(|alias| &alias.name),
        using,
    )?;
    let returning = build_returning_plan(
        state,
        scope.clone(),
        schema.columns.len(),
        delete.returning.as_deref(),
    )?;
    if let Some(selection) = &delete.selection {
        let base = query::infer_query_expression_type(state, selection, &scope)?.base;
        if base != BaseType::Bool && !is_null_literal(selection) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let source_rows = materialize_mutation_source_rows(
        state,
        using,
        &scope,
        schema.columns.len(),
        xid,
        snapshot,
        context,
    )?;
    let targets = collect_mutation_targets(
        state,
        &schema,
        delete.selection.as_ref(),
        &scope,
        &source_rows,
        xid,
        snapshot,
        context,
        mutation_targets,
    )?;
    let affected = targets.len() as u64;
    let mut returned_rows = Vec::new();
    for (row_id, version_xmin, row, bound_row) in targets {
        apply_referencing_foreign_key_actions(
            state,
            &schema,
            &row,
            None,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            &mut BTreeSet::new(),
            context,
        )?;
        state
            .tables
            .get_mut(&schema.id)
            .expect("catalog table must have storage")
            .mark_version_deleted(row_id, version_xmin, xid, context.command_id);
        state.mark_table_touched(xid, schema.id);
        evaluate_returning_row(
            state,
            returning.as_ref(),
            &bound_row,
            &mut returned_rows,
            xid,
            snapshot,
            context,
        )?;
    }
    Ok(create_write_result(affected, returning, returned_rows))
}

type MutationTarget = (RowId, Xid, Vec<Value>, Vec<Value>);

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn collect_mutation_targets(
    state: &DatabaseState,
    schema: &TableSchema,
    selection: Option<&ast::Expr>,
    scope: &BoundScope,
    source_rows: &[Vec<Value>],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    mutation_targets: Option<Vec<RequiredRowLock>>,
) -> Result<Vec<MutationTarget>> {
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    if let Some(mutation_targets) = mutation_targets {
        let [source_row] = source_rows else {
            unreachable!("lock-selected mutations do not have source rows");
        };
        return mutation_targets
            .into_iter()
            .filter(|required| required.key.table_id == schema.id)
            .map(|required| {
                let candidate = required
                    .mutation_candidate
                    .expect("mutation target locks retain their selected row");
                let mut row = source_row.clone();
                row[..schema.columns.len()].clone_from_slice(&candidate.row);
                Ok((
                    required.key.row_id,
                    candidate.version_xmin,
                    candidate.row,
                    row,
                ))
            })
            .collect();
    }
    if let [source_row] = source_rows
        && let Some((column, value)) = super::locks::resolve_unique_point_lookup(
            table,
            schema,
            selection,
            RowScope::Bound(scope),
            context,
        )?
    {
        let Some((row_id, version)) = table.find_unique_visible_version(
            &[column],
            &[value],
            snapshot,
            xid,
            &state.transactions,
        ) else {
            return Ok(Vec::new());
        };
        if version.xmax == Some(xid) && version.xmax_command_id == Some(context.command_id) {
            return Ok(Vec::new());
        }
        let mut row = source_row.clone();
        row[..schema.columns.len()].clone_from_slice(&version.row);
        if matches_mutation_row(state, selection, scope, &row, xid, snapshot, context)? {
            return Ok(vec![(row_id, version.xmin, version.row.clone(), row)]);
        }
        return Ok(Vec::new());
    }
    table
        .iterate_version_chains()
        .try_fold(Vec::new(), |mut targets, (row_id, chain)| {
            let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions)
            else {
                return Ok(targets);
            };
            if version.xmax == Some(xid) && version.xmax_command_id == Some(context.command_id) {
                return Ok(targets);
            }
            for source_row in source_rows {
                let mut row = source_row.clone();
                row[..schema.columns.len()].clone_from_slice(&version.row);
                if matches_mutation_row(state, selection, scope, &row, xid, snapshot, context)? {
                    targets.push((row_id, version.xmin, version.row.clone(), row));
                    break;
                }
            }
            Ok(targets)
        })
}
