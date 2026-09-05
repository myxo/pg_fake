use super::*;

fn require_mutation_table(state: &DatabaseState, name: &RelationName) -> Result<TableSchema> {
    if state.catalog.require_named_view(name).is_ok() {
        return reject_unsupported("mutations targeting views are not implemented");
    }
    Ok(state.catalog.require_named_table(name)?.clone())
}
use crate::catalog::Constraint;
use crate::txn::RowLockAttempt;
use sqlparser::ast;

struct ReturningPlan<'a> {
    scope: BoundScope,
    projections: Vec<query::ProjectionSource<'a>>,
    columns: Vec<ColumnMeta>,
}

struct MutationAssignment<'a> {
    index: usize,
    expression: &'a ast::Expr,
    prepared: Option<prepared::PreparedExpression>,
}

struct ConflictUpdatePlan<'a> {
    scope: BoundScope,
    assigned: BTreeSet<usize>,
    assignments: Vec<MutationAssignment<'a>>,
    selection: Option<&'a ast::Expr>,
}

enum InsertConflictOutcome {
    Insert,
    Skip,
    Update { row_id: RowId, row: Vec<Value> },
}

pub(super) enum ConflictArbiter {
    Any,
    Index {
        columns: Vec<usize>,
        predicate: Option<ast::Expr>,
    },
}

impl ConflictArbiter {
    pub(super) fn get_columns(&self) -> Option<&[usize]> {
        match self {
            ConflictArbiter::Any => None,
            ConflictArbiter::Index { columns, .. } => Some(columns),
        }
    }

    pub(super) fn get_predicate(&self) -> Option<&ast::Expr> {
        match self {
            ConflictArbiter::Any => None,
            ConflictArbiter::Index { predicate, .. } => predicate.as_ref(),
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn resolve_conflict_arbiter(
    schema: &TableSchema,
    on: Option<&ast::OnInsert>,
) -> Result<Option<ConflictArbiter>> {
    let Some(on) = on else {
        return Ok(None);
    };
    let ast::OnInsert::OnConflict(conflict) = on else {
        return reject_unsupported("INSERT conflict action is not implemented");
    };
    let Some(target) = &conflict.conflict_target else {
        if matches!(conflict.action, ast::OnConflictAction::DoUpdate(_)) {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "ON CONFLICT DO UPDATE requires inference specification or constraint name",
            ));
        }
        return Ok(Some(ConflictArbiter::Any));
    };
    let constraint_columns = match target {
        ast::ConflictTarget::Columns { columns, predicate } => {
            let requested = columns
                .iter()
                .map(|column| {
                    let name = normalize_identifier(column);
                    schema
                        .columns
                        .iter()
                        .position(|definition| definition.name == name)
                        .ok_or_else(|| {
                            PgError::create(
                                SqlState::UndefinedColumn,
                                format!("column {name:?} does not exist"),
                            )
                        })
                })
                .collect::<Result<BTreeSet<_>>>()?;
            if requested.len() != columns.len() {
                return Err(PgError::create(
                    SqlState::InvalidColumnReference,
                    "there is no unique or exclusion constraint matching the ON CONFLICT specification",
                ));
            }
            if let Some(predicate) = predicate {
                validate_index_predicate(predicate, schema)?;
            }
            let constraint = schema.constraints.iter().find_map(|constraint| {
                if predicate.is_some() {
                    return None;
                }
                let columns = match constraint {
                    Constraint::PrimaryKey { columns, .. } | Constraint::Unique { columns, .. } => {
                        columns
                    }
                    Constraint::Check { .. } | Constraint::ForeignKey(_) => return None,
                };
                let indexes = columns
                    .iter()
                    .map(|name| {
                        schema
                            .columns
                            .iter()
                            .position(|column| column.name == *name)
                            .expect("constraint columns must exist")
                    })
                    .collect::<Vec<_>>();
                (indexes.len() == requested.len()
                    && indexes.iter().all(|index| requested.contains(index)))
                .then_some((indexes, None))
            });
            constraint.or_else(|| {
                schema.indexes.iter().find_map(|index| {
                    if !index.unique {
                        return None;
                    }
                    let indexes = index
                        .columns
                        .iter()
                        .map(|column| {
                            schema
                                .columns
                                .iter()
                                .position(|definition| definition.name == column.name)
                                .expect("index columns must exist")
                        })
                        .collect::<Vec<_>>();
                    let predicate_matches = match (&index.predicate, predicate) {
                        (None, _) => true,
                        (Some(index), Some(target)) => index == target,
                        (Some(_), None) => false,
                    };
                    (predicate_matches
                        && indexes.len() == requested.len()
                        && indexes.iter().all(|column| requested.contains(column)))
                    .then(|| (indexes, index.predicate.clone()))
                })
            })
        }
        ast::ConflictTarget::OnConstraint(name) => {
            let name = crate::executor::normalize_unqualified_object_name(name)?;
            let Some(constraint) = schema
                .constraints
                .iter()
                .find(|constraint| match constraint {
                    Constraint::PrimaryKey {
                        name: constraint_name,
                        ..
                    }
                    | Constraint::Unique {
                        name: constraint_name,
                        ..
                    } => constraint_name == &name,
                    Constraint::ForeignKey(foreign_key) => foreign_key.name == name,
                    Constraint::Check { .. } => false,
                })
            else {
                return Err(PgError::create(
                    SqlState::UndefinedObject,
                    format!(
                        "constraint {name:?} for table {:?} does not exist",
                        schema.name
                    ),
                ));
            };
            match constraint {
                Constraint::PrimaryKey { columns, .. } | Constraint::Unique { columns, .. } => {
                    Some((
                        columns
                            .iter()
                            .map(|name| {
                                schema
                                    .columns
                                    .iter()
                                    .position(|column| column.name == *name)
                                    .expect("constraint columns must exist")
                            })
                            .collect(),
                        None,
                    ))
                }
                Constraint::Check { .. } | Constraint::ForeignKey(_) => {
                    return Err(PgError::create(
                        SqlState::WrongObjectType,
                        format!("constraint {name:?} has no associated index"),
                    ));
                }
            }
        }
    };
    constraint_columns
        .map(|(columns, predicate)| ConflictArbiter::Index { columns, predicate })
        .map(Some)
        .ok_or_else(|| {
            PgError::create(
                SqlState::InvalidColumnReference,
                "there is no unique or exclusion constraint matching the ON CONFLICT specification",
            )
        })
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
pub(super) fn create_mutation_scope(
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
fn build_mutation_assignments<'a>(
    state: &DatabaseState,
    schema: &TableSchema,
    scope: &BoundScope,
    assignments: &'a [ast::Assignment],
) -> Result<(BTreeSet<usize>, Vec<MutationAssignment<'a>>)> {
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
                    query::infer_query_expression_type(state, &assignment.value, scope)?.base,
                    schema.columns[index].data_type.base,
                    CastContext::Assignment,
                )
            {
                return Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "column has incompatible type",
                ));
            }
            let prepared = if is_default_expression(&assignment.value) {
                None
            } else {
                prepared::bind_prepared_expression(&assignment.value, scope, &[])?.filter(
                    |expression| expression.get_data_type() == schema.columns[index].data_type.base,
                )
            };
            Ok(MutationAssignment {
                index,
                expression: &assignment.value,
                prepared,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((assigned, assignments))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn build_conflict_update_plan<'a>(
    state: &DatabaseState,
    schema: &TableSchema,
    alias: Option<&ast::Ident>,
    update: &'a ast::DoUpdate,
) -> Result<ConflictUpdatePlan<'a>> {
    let target = bind_target_scope(schema, alias);
    let excluded_name = ast::Ident::new("excluded");
    let mut excluded = bind_target_scope(schema, Some(&excluded_name));
    for column in &mut excluded.columns {
        column.unqualified = false;
        column.table_id = None;
    }
    let scope = combine_bound_scopes(target, excluded);
    if let Some(selection) = &update.selection {
        let base = query::infer_query_expression_type(state, selection, &scope)?.base;
        if base != BaseType::Bool && !is_null_literal(selection) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let (assigned, assignments) =
        build_mutation_assignments(state, schema, &scope, &update.assignments)?;
    Ok(ConflictUpdatePlan {
        scope,
        assigned,
        assignments,
        selection: update.selection.as_ref(),
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn resolve_insert_column_indexes(
    schema: &TableSchema,
    columns: &[ast::ObjectName],
) -> Result<Vec<usize>> {
    if columns.is_empty() {
        return Ok((0..schema.columns.len()).collect());
    }
    columns
        .iter()
        .map(|name| {
            let name = normalize_unqualified_object_name(name)?;
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
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_triggered_insert_rows(
    state: &DatabaseState,
    insert: &ast::Insert,
    schema: &TableSchema,
    column_indexes: &[usize],
    returning: Option<&ReturningPlan<'_>>,
    resume: Option<&PreparedTriggerInsert>,
    stop_at_blocking_conflict: bool,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<PreparedTriggerInsert> {
    let provided = column_indexes.iter().copied().collect::<BTreeSet<_>>();
    let static_defaults = schema
        .columns
        .iter()
        .map(|column| {
            let is_static = column.default_sequence.is_none()
                && column.default.as_ref().is_none_or(|expression| {
                    matches!(
                        expression,
                        ast::Expr::Value(value)
                            if !matches!(value.value, ast::Value::Placeholder(_))
                    )
                });
            if is_static {
                evaluate_column_default(column, context).map(Some)
            } else {
                Ok(None)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let evaluate_default = |index: usize| -> Result<Value> {
        static_defaults[index].clone().map_or_else(
            || evaluate_column_default(&schema.columns[index], context),
            Ok,
        )
    };
    let build_row = |expressions: &[ast::Expr]| -> Result<Vec<Value>> {
        if expressions.len() != column_indexes.len() {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "INSERT has wrong number of values",
            ));
        }
        let mut row = vec![Value::Null; schema.columns.len()];
        for index in 0..schema.columns.len() {
            if !provided.contains(&index) {
                row[index] = evaluate_default(index)?;
            }
        }
        let constants = create_constant_expression_schema();
        for (expression, index) in expressions.iter().zip(column_indexes) {
            if schema.columns[*index].identity == Some(IdentityKind::Always)
                && !is_default_expression(expression)
            {
                return Err(PgError::create(
                    SqlState::GeneratedAlways,
                    format!(
                        "cannot insert a non-DEFAULT value into column {:?}",
                        schema.columns[*index].name
                    ),
                ));
            }
            row[*index] = if is_default_expression(expression) {
                evaluate_default(*index)?
            } else {
                evaluate_assignment_expression(
                    expression,
                    schema.columns[*index].data_type,
                    &constants,
                    &[],
                    context,
                )?
            };
        }
        Ok(row)
    };
    let execute_triggers = |row| -> Result<Option<Vec<Value>>> {
        let Some(row) = procedural::execute_before_row_triggers(
            state,
            schema,
            procedural::TriggerEventKind::Insert,
            row,
            context,
        )?
        else {
            return Ok(None);
        };
        validate_not_null(schema, &row)?;
        validate_check_constraints(schema, &row, context)?;
        Ok(Some(row))
    };
    let validate_prepared_row = |row: &Vec<Value>, prior: &[Vec<Value>]| -> Result<()> {
        if insert.on.is_none() {
            let table = state
                .tables
                .get(&schema.id)
                .expect("catalog table must have storage");
            if table.has_visible_unique_conflict(
                row,
                snapshot,
                xid,
                &state.transactions,
                None,
                None,
                None,
                None,
                context,
            ) || prior
                .iter()
                .any(|previous| table.rows_have_unique_conflict(previous, row, context))
            {
                return Err(PgError::create(
                    SqlState::UniqueViolation,
                    format!(
                        "duplicate key value violates unique constraint on {:?}",
                        schema.name
                    ),
                ));
            }
        }
        Ok(())
    };
    let conflict_arbiter = resolve_conflict_arbiter(schema, insert.on.as_ref())?;
    let conflict_update = match insert.on.as_ref() {
        Some(ast::OnInsert::OnConflict(ast::OnConflict {
            action: ast::OnConflictAction::DoUpdate(update),
            ..
        })) => Some(build_conflict_update_plan(
            state,
            schema,
            insert.table_alias.as_ref().map(|alias| &alias.alias),
            update,
        )?),
        _ => None,
    };
    if let Some(update) = &conflict_update {
        for assignment in &update.assignments {
            if is_default_expression(assignment.expression) {
                continue;
            }
            if let Some(prepared) =
                prepared::bind_prepared_expression(assignment.expression, &update.scope, &[])?
                && prepared.is_constant()
            {
                prepared::evaluate_prepared_expression(&prepared, &[], &[], context.deadline)?;
            }
        }
    }
    let prepares_returning = returning.is_some();
    let mut validation_table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .clone();
    let mut affected_rows = BTreeSet::new();
    for (prior_insert, prepared) in stop_at_blocking_conflict
        .then(|| context.get_prior_prepared_trigger_inserts(insert))
        .unwrap_or_default()
    {
        let prior_schema = state
            .catalog
            .require_named_table(&resolve_insert_table_name(&prior_insert.table)?)?;
        if prior_schema.id != schema.id {
            continue;
        }
        let prior_arbiter = resolve_conflict_arbiter(prior_schema, prior_insert.on.as_ref())?;
        let prior_updates_conflicts = matches!(
            prior_insert.on,
            Some(ast::OnInsert::OnConflict(ast::OnConflict {
                action: ast::OnConflictAction::DoUpdate(_),
                ..
            }))
        );
        for (row, conflict) in prepared.rows.iter().zip(&prepared.conflicts) {
            match conflict {
                Some(prepared) => {
                    if let Some(updated) = &prepared.updated {
                        validation_table.append_updated_version(
                            prepared.row_id,
                            prepared.version_xmin,
                            xid,
                            context.command_id,
                            updated.clone(),
                            None,
                        );
                        affected_rows.insert(prepared.row_id);
                    }
                }
                None if !prior_updates_conflicts
                    && prior_arbiter.as_ref().is_some_and(|arbiter| {
                        validation_table.has_visible_unique_conflict(
                            row,
                            snapshot,
                            xid,
                            &state.transactions,
                            None,
                            None,
                            arbiter.get_columns(),
                            arbiter.get_predicate(),
                            context,
                        )
                    }) => {}
                None => {
                    let row_id = validation_table.insert(xid, context.command_id, row.clone());
                    affected_rows.insert(row_id);
                }
            }
        }
    }
    let stops_at_blocking_conflict = std::cell::Cell::new(stop_at_blocking_conflict);
    let stopped = std::cell::Cell::new(false);
    let mut prepare_row = |prepared_row_index: usize,
                           row: Vec<Value>,
                           rows: &mut Vec<Vec<Value>>,
                           conflicts: &mut Vec<Option<PreparedConflictUpdate>>,
                           returned_rows: &mut Vec<Option<Vec<Value>>>|
     -> Result<()> {
        validate_prepared_row(&row, rows)?;
        if let Some(cached_conflict) =
            resume.and_then(|cached| cached.conflicts.get(prepared_row_index))
        {
            let skips_do_nothing_conflict = conflict_arbiter.as_ref().is_some_and(|arbiter| {
                conflict_update.is_none()
                    && validation_table.has_visible_unique_conflict(
                        &row,
                        snapshot,
                        xid,
                        &state.transactions,
                        None,
                        None,
                        arbiter.get_columns(),
                        arbiter.get_predicate(),
                        context,
                    )
            });
            match cached_conflict {
                Some(prepared) => {
                    if let Some(updated) = &prepared.updated {
                        validation_table.append_updated_version(
                            prepared.row_id,
                            prepared.version_xmin,
                            xid,
                            context.command_id,
                            updated.clone(),
                            None,
                        );
                        affected_rows.insert(prepared.row_id);
                    }
                }
                None if skips_do_nothing_conflict => {}
                None => {
                    let row_id = validation_table.insert(xid, context.command_id, row.clone());
                    affected_rows.insert(row_id);
                }
            }
            rows.push(row);
            conflicts.push(cached_conflict.clone());
            if prepares_returning {
                returned_rows.push(
                    resume
                        .and_then(|cached| cached.returned_rows.as_ref())
                        .and_then(|cached| cached.get(prepared_row_index))
                        .cloned()
                        .unwrap_or(None),
                );
            }
            return Ok(());
        }
        let blocking_conflict = conflict_arbiter.as_ref().and_then(|arbiter| {
            state
                .tables
                .get(&schema.id)
                .expect("catalog table must have storage")
                .find_conflicting_row(
                    &row,
                    xid,
                    &state.transactions,
                    arbiter.get_columns(),
                    arbiter.get_predicate(),
                    context,
                )
                .filter(|row_id| {
                    state.row_locks.would_block(
                        RowLockKey {
                            table_id: schema.id,
                            row_id: *row_id,
                        },
                        xid,
                        RowLockMode::Update,
                    )
                })
        });
        if blocking_conflict.is_some() {
            rows.push(row);
            if stops_at_blocking_conflict.get() {
                stopped.set(true);
            } else {
                conflicts.push(None);
                if prepares_returning {
                    returned_rows.push(None);
                }
            }
            return Ok(());
        }
        let prepared_conflict = prepare_triggered_conflict_update(
            state,
            schema,
            &validation_table,
            &row,
            conflict_arbiter.as_ref(),
            conflict_update.as_ref(),
            Some(&affected_rows),
            xid,
            snapshot,
            context,
        )?;
        let skips_do_nothing_conflict = conflict_arbiter.as_ref().is_some_and(|arbiter| {
            conflict_update.is_none()
                && validation_table.has_visible_unique_conflict(
                    &row,
                    snapshot,
                    xid,
                    &state.transactions,
                    None,
                    None,
                    arbiter.get_columns(),
                    arbiter.get_predicate(),
                    context,
                )
        });
        let returned_row = match &prepared_conflict {
            Some(prepared) => match &prepared.updated {
                Some(updated) => {
                    validate_not_null(schema, updated)?;
                    validate_check_constraints(schema, updated, context)?;
                    Some(updated.clone())
                }
                None => None,
            },
            None if skips_do_nothing_conflict => None,
            None => Some(row.clone()),
        };
        match &prepared_conflict {
            Some(prepared) => {
                if let Some(updated) = &prepared.updated {
                    if validation_table.has_visible_unique_conflict(
                        updated,
                        snapshot,
                        xid,
                        &state.transactions,
                        Some(prepared.row_id),
                        None,
                        None,
                        None,
                        context,
                    ) {
                        return Err(PgError::create(
                            SqlState::UniqueViolation,
                            format!(
                                "duplicate key value violates unique constraint on {:?}",
                                schema.name
                            ),
                        ));
                    }
                    validation_table.append_updated_version(
                        prepared.row_id,
                        prepared.version_xmin,
                        xid,
                        context.command_id,
                        updated.clone(),
                        None,
                    );
                    affected_rows.insert(prepared.row_id);
                }
            }
            None if skips_do_nothing_conflict => {}
            None => {
                if validation_table.has_visible_unique_conflict(
                    &row,
                    snapshot,
                    xid,
                    &state.transactions,
                    None,
                    None,
                    None,
                    None,
                    context,
                ) {
                    return Err(PgError::create(
                        SqlState::UniqueViolation,
                        format!(
                            "duplicate key value violates unique constraint on {:?}",
                            schema.name
                        ),
                    ));
                }
                let row_id = validation_table.insert(xid, context.command_id, row.clone());
                affected_rows.insert(row_id);
            }
        }
        rows.push(row);
        conflicts.push(prepared_conflict);
        if let (true, Some(returned_row)) = (prepares_returning, returned_row) {
            let mut evaluated = Vec::new();
            evaluate_returning_row(
                state,
                returning,
                &returned_row,
                &mut evaluated,
                xid,
                snapshot,
                context,
            )?;
            returned_rows.push(Some(
                evaluated
                    .pop()
                    .expect("RETURNING evaluation produces one row"),
            ));
        } else if prepares_returning {
            returned_rows.push(None);
        }
        Ok(())
    };
    let prepared_returning = |rows| prepares_returning.then_some(rows);
    let Some(source) = &insert.source else {
        assert!(insert.columns.is_empty());
        let evaluated = match resume.and_then(|cached| cached.source_rows.first()) {
            Some(row) => Ok(row.clone()),
            None => schema
                .columns
                .iter()
                .enumerate()
                .map(|(index, _)| evaluate_default(index))
                .collect::<Result<Vec<_>>>()
                .and_then(&execute_triggers),
        };
        let mut rows = Vec::new();
        let mut conflicts = Vec::new();
        let mut returned_rows = Vec::new();
        let error = match &evaluated {
            Ok(Some(row)) => prepare_row(
                0,
                row.clone(),
                &mut rows,
                &mut conflicts,
                &mut returned_rows,
            )
            .err(),
            Ok(None) => None,
            Err(error) => Some(error.clone()),
        };
        return Ok(PreparedTriggerInsert {
            source_state: None,
            source_snapshot: None,
            source_query: None,
            source_rows: evaluated.ok().into_iter().collect(),
            rows,
            conflicts,
            returned_rows: prepared_returning(returned_rows),
            error,
            complete: !stopped.get(),
        });
    };
    if let ast::SetExpr::Values(values) = source.body.as_ref() {
        let mut source_rows = Vec::new();
        let mut rows = Vec::new();
        let mut conflicts = Vec::new();
        let mut returned_rows = Vec::new();
        for (row_index, expressions) in values.rows.iter().enumerate() {
            let evaluated = match resume.and_then(|cached| cached.source_rows.get(row_index)) {
                Some(row) => Ok(row.clone()),
                None => build_row(expressions).and_then(&execute_triggers),
            };
            match &evaluated {
                Ok(Some(row)) => {
                    let prepared_row_index = rows.len();
                    if let Err(error) = prepare_row(
                        prepared_row_index,
                        row.clone(),
                        &mut rows,
                        &mut conflicts,
                        &mut returned_rows,
                    ) {
                        source_rows.push(evaluated.expect("evaluated row is successful"));
                        return Ok(PreparedTriggerInsert {
                            source_state: None,
                            source_snapshot: None,
                            source_query: None,
                            source_rows,
                            rows,
                            conflicts,
                            returned_rows: prepared_returning(returned_rows),
                            error: Some(error),
                            complete: true,
                        });
                    }
                    source_rows.push(evaluated.expect("evaluated row is successful"));
                    if stopped.get() {
                        break;
                    }
                }
                Ok(None) => source_rows.push(None),
                Err(error) => {
                    return Ok(PreparedTriggerInsert {
                        source_state: None,
                        source_snapshot: None,
                        source_query: None,
                        source_rows,
                        rows,
                        conflicts,
                        returned_rows: prepared_returning(returned_rows),
                        error: Some(error.clone()),
                        complete: true,
                    });
                }
            }
        }
        return Ok(PreparedTriggerInsert {
            source_state: None,
            source_snapshot: None,
            source_query: None,
            source_rows,
            rows,
            conflicts,
            returned_rows: prepared_returning(returned_rows),
            error: None,
            complete: !stopped.get(),
        });
    }
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
    let mut streamed_source_rows = Vec::new();
    let mut streamed_rows = Vec::new();
    let mut streamed_conflicts = Vec::new();
    let mut streamed_returned_rows = Vec::new();
    let mut streamed_error = None;
    let mut source_query = resume.and_then(|cached| cached.source_query.clone());
    let source_snapshot = resume
        .and_then(|cached| cached.source_snapshot.as_ref())
        .unwrap_or(&context.source_snapshot);
    let source_state = resume
        .and_then(|cached| cached.source_state.clone())
        .or_else(|| context.source_state.clone())
        .unwrap_or_else(|| Arc::new(state.clone()));
    if let Some(resume) = resume {
        for cached in &resume.source_rows {
            if let Some(row) = cached {
                let row_index = streamed_rows.len();
                if let Err(error) = prepare_row(
                    row_index,
                    row.clone(),
                    &mut streamed_rows,
                    &mut streamed_conflicts,
                    &mut streamed_returned_rows,
                ) {
                    streamed_source_rows.push(cached.clone());
                    return Ok(PreparedTriggerInsert {
                        source_state: Some(source_state.clone()),
                        source_snapshot: Some(source_snapshot.clone()),
                        source_query,
                        source_rows: streamed_source_rows,
                        rows: streamed_rows,
                        conflicts: streamed_conflicts,
                        returned_rows: prepared_returning(streamed_returned_rows),
                        error: Some(error),
                        complete: true,
                    });
                }
            }
            streamed_source_rows.push(cached.clone());
            if stopped.get() {
                return Ok(PreparedTriggerInsert {
                    source_state: Some(source_state.clone()),
                    source_snapshot: Some(source_snapshot.clone()),
                    source_query,
                    source_rows: streamed_source_rows,
                    rows: streamed_rows,
                    conflicts: streamed_conflicts,
                    returned_rows: prepared_returning(streamed_returned_rows),
                    error: None,
                    complete: false,
                });
            }
        }
    }
    let streamed = query::stream_plain_query_rows(
        &source_state,
        source,
        xid,
        source_snapshot,
        context,
        None,
        &mut source_query,
        &mut |values, columns| {
            if columns.len() != column_indexes.len() {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "INSERT has wrong number of values",
                ));
            }
            let source_index = streamed_source_rows.len();
            let evaluated = match resume.and_then(|cached| cached.source_rows.get(source_index)) {
                Some(row) => Ok(row.clone()),
                None => (|| {
                    let mut row = vec![Value::Null; schema.columns.len()];
                    for index in 0..schema.columns.len() {
                        if !provided.contains(&index) {
                            row[index] = evaluate_default(index)?;
                        }
                    }
                    for (((value, source_column), unknown), index) in values
                        .into_iter()
                        .zip(columns)
                        .zip(&unknown_columns)
                        .zip(column_indexes)
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
                    execute_triggers(row)
                })(),
            };
            match &evaluated {
                Ok(Some(row)) => {
                    let row_index = streamed_rows.len();
                    if let Err(error) = prepare_row(
                        row_index,
                        row.clone(),
                        &mut streamed_rows,
                        &mut streamed_conflicts,
                        &mut streamed_returned_rows,
                    ) {
                        streamed_error = Some(error.clone());
                        return Err(error);
                    }
                    streamed_source_rows.push(evaluated.expect("evaluated row is successful"));
                    if stopped.get() {
                        return Err(PgError::create(
                            SqlState::QueryCanceled,
                            "trigger INSERT preparation stopped",
                        ));
                    }
                }
                Ok(None) => streamed_source_rows.push(None),
                Err(error) => {
                    streamed_error = Some(error.clone());
                    return Err(error.clone());
                }
            }
            Ok(())
        },
    );
    match streamed {
        Err(_) if stopped.get() => {
            return Ok(PreparedTriggerInsert {
                source_state: Some(source_state.clone()),
                source_snapshot: Some(source_snapshot.clone()),
                source_query,
                source_rows: streamed_source_rows,
                rows: streamed_rows,
                conflicts: streamed_conflicts,
                returned_rows: prepared_returning(streamed_returned_rows),
                error: None,
                complete: false,
            });
        }
        Err(error) => {
            return match streamed_error {
                Some(error) => Ok(PreparedTriggerInsert {
                    source_state: Some(source_state.clone()),
                    source_snapshot: Some(source_snapshot.clone()),
                    source_query,
                    source_rows: streamed_source_rows,
                    rows: streamed_rows,
                    conflicts: streamed_conflicts,
                    returned_rows: prepared_returning(streamed_returned_rows),
                    error: Some(error),
                    complete: true,
                }),
                None => Err(error),
            };
        }
        Ok(Some(columns)) => {
            if columns.len() != column_indexes.len() {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "INSERT has wrong number of values",
                ));
            }
            return Ok(PreparedTriggerInsert {
                source_state: Some(source_state.clone()),
                source_snapshot: Some(source_snapshot.clone()),
                source_query,
                source_rows: streamed_source_rows,
                rows: streamed_rows,
                conflicts: streamed_conflicts,
                returned_rows: prepared_returning(streamed_returned_rows),
                error: None,
                complete: true,
            });
        }
        Ok(None) => {}
    }
    let Some(query::PreparedQueryStream::Materialized { result: source, .. }) =
        source_query.as_ref()
    else {
        unreachable!("non-streamable INSERT source is materialized")
    };
    if source.columns.len() != column_indexes.len() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "INSERT has wrong number of values",
        ));
    }
    for values in source.rows.iter().skip(streamed_source_rows.len()) {
        let evaluated = (|| -> Result<Option<Vec<Value>>> {
            let mut row = vec![Value::Null; schema.columns.len()];
            for index in 0..schema.columns.len() {
                if !provided.contains(&index) {
                    row[index] = evaluate_default(index)?;
                }
            }
            for (((value, source_column), unknown), index) in values
                .iter()
                .cloned()
                .zip(&source.columns)
                .zip(&unknown_columns)
                .zip(column_indexes)
            {
                let value = if *unknown {
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
                row[*index] = value;
            }
            execute_triggers(row)
        })();
        match evaluated {
            Ok(Some(row)) => {
                let row_index = streamed_rows.len();
                if let Err(error) = prepare_row(
                    row_index,
                    row.clone(),
                    &mut streamed_rows,
                    &mut streamed_conflicts,
                    &mut streamed_returned_rows,
                ) {
                    streamed_source_rows.push(Some(row));
                    return Ok(PreparedTriggerInsert {
                        source_state: Some(source_state.clone()),
                        source_snapshot: Some(source_snapshot.clone()),
                        source_query,
                        source_rows: streamed_source_rows,
                        rows: streamed_rows,
                        conflicts: streamed_conflicts,
                        returned_rows: prepared_returning(streamed_returned_rows),
                        error: Some(error),
                        complete: true,
                    });
                }
                streamed_source_rows.push(Some(row));
                if stopped.get() {
                    break;
                }
            }
            Ok(None) => streamed_source_rows.push(None),
            Err(error) => {
                return Ok(PreparedTriggerInsert {
                    source_state: Some(source_state.clone()),
                    source_snapshot: Some(source_snapshot.clone()),
                    source_query,
                    source_rows: streamed_source_rows,
                    rows: streamed_rows,
                    conflicts: streamed_conflicts,
                    returned_rows: prepared_returning(streamed_returned_rows),
                    error: Some(error),
                    complete: true,
                });
            }
        }
    }
    Ok(PreparedTriggerInsert {
        source_state: Some(source_state),
        source_snapshot: Some(source_snapshot.clone()),
        source_query,
        source_rows: streamed_source_rows,
        rows: streamed_rows,
        conflicts: streamed_conflicts,
        returned_rows: prepared_returning(streamed_returned_rows),
        error: None,
        complete: !stopped.get(),
    })
}

pub(super) fn preview_triggered_insert_rows(
    state: &DatabaseState,
    insert: &ast::Insert,
    schema: &TableSchema,
    column_indexes: &[usize],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<(Vec<Vec<Value>>, Vec<Option<PreparedConflictUpdate>>)> {
    let returning_scope = bind_target_scope(
        schema,
        insert.table_alias.as_ref().map(|alias| &alias.alias),
    );
    let returning = build_returning_plan(
        state,
        returning_scope,
        schema.columns.len(),
        insert.returning.as_deref(),
    )?;
    let resume = context.get_prepared_trigger_insert(insert);
    let prepared = match resume {
        Some(prepared) if prepared.complete => prepared,
        resume => evaluate_triggered_insert_rows(
            state,
            insert,
            schema,
            column_indexes,
            returning.as_ref(),
            resume.as_ref(),
            true,
            xid,
            snapshot,
            context,
        )?,
    };
    context.set_prepared_trigger_insert(insert, prepared.clone());
    if !prepared.complete {
        context.request_trigger_lock_recheck();
    } else if let Some(error) = prepared.error.clone() {
        return Err(error);
    }
    Ok((prepared.rows, prepared.conflicts))
}

pub(super) fn collect_update_cte_locks(
    state: &DatabaseState,
    update: &ast::Update,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<RequiredRowLock>> {
    let ast::TableFactor::Table {
        name: table_name,
        alias,
        args: None,
        ..
    } = &update.table.relation
    else {
        return Ok(Vec::new());
    };
    let schema = state
        .catalog
        .require_named_table(&normalize_relation_name(table_name)?)?;
    let from = match &update.from {
        None => &[][..],
        Some(ast::UpdateTableFromKind::AfterSet(from)) => from.as_slice(),
        Some(ast::UpdateTableFromKind::BeforeSet(_)) => return Ok(Vec::new()),
    };
    let scope =
        create_mutation_scope(state, schema, alias.as_ref().map(|alias| &alias.name), from)?;
    let occurrence = update.span();
    let sql = update.to_string();
    let targets = match context.get_prepared_mutation_targets(occurrence, snapshot.commit_seq) {
        Some(targets) => targets,
        None => {
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
                schema,
                update.selection.as_ref(),
                &scope,
                &source_rows,
                xid,
                snapshot,
                context,
                None,
            )?;
            validate_mutation_target_versions(state, schema, &targets, xid, snapshot)?;
            let targets = targets
                .into_iter()
                .map(
                    |(row_id, version_xmin, current, bound_row)| PreparedMutationTarget {
                        row_id,
                        version_xmin,
                        current,
                        bound_row,
                    },
                )
                .collect::<Vec<_>>();
            context.set_prepared_mutation_targets(
                occurrence,
                sql,
                snapshot.commit_seq,
                targets.clone(),
            );
            targets
        }
    };
    let mut locks = targets
        .iter()
        .map(|target| RequiredRowLock {
            key: RowLockKey {
                table_id: schema.id,
                row_id: target.row_id,
            },
            mode: RowLockMode::Update,
            mutation_candidate: Some(MutationCandidate {
                version_xmin: target.version_xmin,
                row: Some(target.current.clone()),
            }),
        })
        .collect::<Vec<_>>();
    if locks
        .iter()
        .any(|lock| !state.row_locks.is_held(lock.key, xid, lock.mode))
    {
        context.request_trigger_lock_recheck_with_locks(locks.clone());
        return Ok(locks);
    }
    if schema
        .constraints
        .iter()
        .any(|constraint| matches!(constraint, Constraint::ForeignKey(_)))
    {
        let prepared =
            prepare_triggered_update_rows(state, update, schema, xid, snapshot, context)?;
        let foreign_key_locks = super::locks::collect_foreign_key_locks_for_rows(
            state,
            schema,
            prepared.iter().filter_map(|row| row.updated.as_ref()),
            xid,
        )?;
        if foreign_key_locks
            .iter()
            .any(|lock| !state.row_locks.is_held(lock.key, xid, lock.mode))
        {
            context.request_trigger_lock_recheck_with_locks(foreign_key_locks.clone());
        }
        locks.extend(foreign_key_locks);
    }
    Ok(locks)
}

pub(super) fn collect_delete_cte_locks(
    state: &DatabaseState,
    delete: &ast::Delete,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<RequiredRowLock>> {
    let ast::FromTable::WithFromKeyword(from) = &delete.from else {
        return Ok(Vec::new());
    };
    let [target] = from.as_slice() else {
        return Ok(Vec::new());
    };
    let ast::TableFactor::Table {
        name: table_name,
        alias,
        args: None,
        ..
    } = &target.relation
    else {
        return Ok(Vec::new());
    };
    if !target.joins.is_empty() {
        return Ok(Vec::new());
    }
    let schema = state
        .catalog
        .require_named_table(&normalize_relation_name(table_name)?)?;
    let using = delete.using.as_deref().unwrap_or_default();
    let scope = create_mutation_scope(
        state,
        schema,
        alias.as_ref().map(|alias| &alias.name),
        using,
    )?;
    let occurrence = delete.span();
    let sql = delete.to_string();
    let targets = match context.get_prepared_mutation_targets(occurrence, snapshot.commit_seq) {
        Some(targets) => targets,
        None => {
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
                schema,
                delete.selection.as_ref(),
                &scope,
                &source_rows,
                xid,
                snapshot,
                context,
                None,
            )?;
            validate_mutation_target_versions(state, schema, &targets, xid, snapshot)?;
            let targets = targets
                .into_iter()
                .map(
                    |(row_id, version_xmin, current, bound_row)| PreparedMutationTarget {
                        row_id,
                        version_xmin,
                        current,
                        bound_row,
                    },
                )
                .collect::<Vec<_>>();
            context.set_prepared_mutation_targets(
                occurrence,
                sql,
                snapshot.commit_seq,
                targets.clone(),
            );
            targets
        }
    };
    let locks = targets
        .iter()
        .map(|target| RequiredRowLock {
            key: RowLockKey {
                table_id: schema.id,
                row_id: target.row_id,
            },
            mode: RowLockMode::Update,
            mutation_candidate: Some(MutationCandidate {
                version_xmin: target.version_xmin,
                row: Some(target.current.clone()),
            }),
        })
        .collect::<Vec<_>>();
    if locks
        .iter()
        .any(|lock| !state.row_locks.is_held(lock.key, xid, lock.mode))
    {
        context.request_trigger_lock_recheck_with_locks(locks.clone());
    }
    Ok(locks)
}

fn validate_mutation_target_versions(
    state: &DatabaseState,
    schema: &TableSchema,
    targets: &[MutationTarget],
    xid: Xid,
    snapshot: &Snapshot,
) -> Result<()> {
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    for (row_id, version_xmin, _, _) in targets {
        let (_, chain) = table
            .iterate_version_chains()
            .find(|(candidate, _)| candidate == row_id)
            .expect("selected mutation row must exist");
        let version = chain
            .versions
            .iter()
            .find(|version| version.xmin == *version_xmin)
            .expect("selected mutation version must exist");
        super::locks::check_concurrent_update(state, version, xid, snapshot)?;
    }
    Ok(())
}

fn prepare_triggered_conflict_update(
    state: &DatabaseState,
    schema: &TableSchema,
    table: &Table,
    row: &Vec<Value>,
    arbiter: Option<&ConflictArbiter>,
    update: Option<&ConflictUpdatePlan<'_>>,
    affected_rows: Option<&BTreeSet<RowId>>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Option<PreparedConflictUpdate>> {
    let (Some(arbiter), Some(update)) = (arbiter, update) else {
        return Ok(None);
    };
    let columns = arbiter
        .get_columns()
        .expect("DO UPDATE requires a unique arbiter");
    let Some((row_id, version)) = table.find_visible_unique_conflict(
        row,
        snapshot,
        xid,
        &state.transactions,
        columns,
        arbiter.get_predicate(),
        context,
    ) else {
        return Ok(None);
    };
    if affected_rows.is_some_and(|affected| affected.contains(&row_id)) {
        return Err(PgError::create(
            SqlState::CardinalityViolation,
            "ON CONFLICT DO UPDATE command cannot affect row a second time",
        ));
    }
    let current = version.row.clone();
    let mut bound_row = current.clone();
    bound_row.extend_from_slice(row);
    if !matches_mutation_row(
        state,
        update.selection,
        &update.scope,
        &bound_row,
        xid,
        snapshot,
        context,
    )? {
        return Ok(Some(PreparedConflictUpdate {
            row_id,
            version_xmin: version.xmin,
            current,
            updated: None,
        }));
    }
    let mut updated = current.clone();
    for assignment in &update.assignments {
        updated[assignment.index] = if is_default_expression(assignment.expression) {
            evaluate_column_default(&schema.columns[assignment.index], context)?
        } else if let Some(prepared) = &assignment.prepared {
            prepared::evaluate_prepared_expression(prepared, &bound_row, &[], context.deadline)?
        } else {
            evaluate_mutation_assignment(
                state,
                assignment.expression,
                schema.columns[assignment.index].data_type,
                &update.scope,
                &bound_row,
                xid,
                snapshot,
                context,
            )?
        };
    }
    let updated = procedural::execute_before_row_triggers(
        state,
        schema,
        procedural::TriggerEventKind::Update,
        updated,
        context,
    )?;
    Ok(Some(PreparedConflictUpdate {
        row_id,
        version_xmin: version.xmin,
        current,
        updated,
    }))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_insert_conflict(
    state: &mut DatabaseState,
    schema: &TableSchema,
    row: &Vec<Value>,
    arbiter: Option<&ConflictArbiter>,
    update: Option<&ConflictUpdatePlan<'_>>,
    affected_rows: &BTreeSet<RowId>,
    has_referencing_foreign_keys: bool,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementExecutionContext,
    prepared: Option<&PreparedConflictUpdate>,
) -> Result<InsertConflictOutcome> {
    let Some(arbiter) = arbiter else {
        return Ok(InsertConflictOutcome::Insert);
    };
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    let Some(update) = update else {
        return Ok(
            if table.has_visible_unique_conflict(
                row,
                snapshot,
                xid,
                &state.transactions,
                None,
                None,
                arbiter.get_columns(),
                arbiter.get_predicate(),
                context,
            ) {
                InsertConflictOutcome::Skip
            } else {
                InsertConflictOutcome::Insert
            },
        );
    };
    if let Some(prepared) = prepared {
        if affected_rows.contains(&prepared.row_id) {
            return Err(PgError::create(
                SqlState::CardinalityViolation,
                "ON CONFLICT DO UPDATE command cannot affect row a second time",
            ));
        }
        let Some(updated) = &prepared.updated else {
            return Ok(InsertConflictOutcome::Skip);
        };
        if state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .has_visible_unique_conflict(
                updated,
                snapshot,
                xid,
                &state.transactions,
                Some(prepared.row_id),
                schema.triggers.is_empty().then_some(&update.assigned),
                None,
                None,
                context,
            )
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
                prepared.row_id,
                prepared.version_xmin,
                xid,
                context.command_id,
                updated.clone(),
                schema.triggers.is_empty().then_some(&update.assigned),
            );
        state.mark_table_touched(xid, schema.id);
        validate_row_foreign_keys(
            state,
            schema,
            updated,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            &[],
        )?;
        if has_referencing_foreign_keys {
            apply_referencing_foreign_key_actions(
                state,
                schema,
                &prepared.current,
                Some(updated),
                xid,
                snapshot,
                deferred_constraints,
                defer_all,
                &mut BTreeSet::new(),
                context,
            )?;
        }
        return Ok(InsertConflictOutcome::Update {
            row_id: prepared.row_id,
            row: updated.clone(),
        });
    }
    let columns = arbiter
        .get_columns()
        .expect("DO UPDATE requires a unique arbiter");
    let conflict = table
        .find_visible_unique_conflict(
            row,
            snapshot,
            xid,
            &state.transactions,
            columns,
            arbiter.get_predicate(),
            context,
        )
        .map(|(row_id, version)| (row_id, version.xmin, version.row.clone()));
    let Some((row_id, version_xmin, current)) = conflict else {
        return Ok(InsertConflictOutcome::Insert);
    };
    let mut bound_row = current.clone();
    bound_row.extend_from_slice(row);
    if !matches_mutation_row(
        state,
        update.selection,
        &update.scope,
        &bound_row,
        xid,
        snapshot,
        context,
    )? {
        return Ok(InsertConflictOutcome::Skip);
    }
    if affected_rows.contains(&row_id) {
        return Err(PgError::create(
            SqlState::CardinalityViolation,
            "ON CONFLICT DO UPDATE command cannot affect row a second time",
        ));
    }
    let mut updated = current.clone();
    for assignment in &update.assignments {
        updated[assignment.index] = if is_default_expression(assignment.expression) {
            evaluate_column_default(&schema.columns[assignment.index], context)?
        } else if let Some(prepared) = &assignment.prepared {
            prepared::evaluate_prepared_expression(prepared, &bound_row, &[], context.deadline)?
        } else {
            evaluate_mutation_assignment(
                state,
                assignment.expression,
                schema.columns[assignment.index].data_type,
                &update.scope,
                &bound_row,
                xid,
                snapshot,
                context,
            )?
        };
    }
    let Some(updated) = procedural::execute_before_row_triggers(
        state,
        schema,
        procedural::TriggerEventKind::Update,
        updated,
        context,
    )?
    else {
        return Ok(InsertConflictOutcome::Skip);
    };
    validate_not_null(schema, &updated)?;
    validate_check_constraints(schema, &updated, context)?;
    if state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .has_visible_unique_conflict(
            &updated,
            snapshot,
            xid,
            &state.transactions,
            Some(row_id),
            schema.triggers.is_empty().then_some(&update.assigned),
            None,
            None,
            context,
        )
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
            schema.triggers.is_empty().then_some(&update.assigned),
        );
    state.mark_table_touched(xid, schema.id);
    validate_row_foreign_keys(
        state,
        schema,
        &updated,
        xid,
        snapshot,
        deferred_constraints,
        defer_all,
        &[],
    )?;
    if has_referencing_foreign_keys {
        apply_referencing_foreign_key_actions(
            state,
            schema,
            &current,
            Some(&updated),
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            &mut BTreeSet::new(),
            context,
        )?;
    }
    Ok(InsertConflictOutcome::Update {
        row_id,
        row: updated,
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn insert_new_row(
    state: &mut DatabaseState,
    schema: &TableSchema,
    row: Vec<Value>,
    can_move_row: bool,
    returning: Option<&ReturningPlan<'_>>,
    returned_rows: &mut Vec<Vec<Value>>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<RowId> {
    let retained_row = (!can_move_row).then(|| row.clone());
    let row_id = state
        .tables
        .get_mut(&schema.id)
        .expect("catalog table must have storage")
        .insert(xid, context.command_id, row);
    let lock = state.row_locks.acquire(
        RowLockKey {
            table_id: schema.id,
            row_id,
        },
        xid,
        RowLockMode::Update,
    );
    assert!(matches!(lock, RowLockAttempt::Acquired));
    state.mark_table_touched(xid, schema.id);
    if let Some(row) = retained_row {
        evaluate_returning_row(
            state,
            returning,
            &row,
            returned_rows,
            xid,
            snapshot,
            context,
        )?;
    }
    Ok(row_id)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_insert(
    state: &mut DatabaseState,
    insert: &ast::Insert,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    let table_name = resolve_insert_table_name(&insert.table)?;
    let schema = require_mutation_table(state, &table_name)?;
    let conflict_arbiter = resolve_conflict_arbiter(&schema, insert.on.as_ref())?;
    let conflict_update = match insert.on.as_ref() {
        Some(ast::OnInsert::OnConflict(ast::OnConflict {
            action: ast::OnConflictAction::DoUpdate(update),
            ..
        })) => Some(build_conflict_update_plan(
            state,
            &schema,
            insert.table_alias.as_ref().map(|alias| &alias.alias),
            update,
        )?),
        _ => None,
    };
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
    let column_indexes = resolve_insert_column_indexes(&schema, &insert.columns)?;
    let (rows, prepared_conflicts, prepared_returned_rows, prepared_error) =
        match context.take_prepared_trigger_insert(insert) {
            Some(prepared) => (
                prepared.rows,
                prepared.conflicts,
                prepared.returned_rows,
                prepared.error,
            ),
            None => {
                let prepared = evaluate_triggered_insert_rows(
                    state,
                    insert,
                    &schema,
                    &column_indexes,
                    returning.as_ref(),
                    None,
                    false,
                    xid,
                    snapshot,
                    context,
                )?;
                (
                    prepared.rows,
                    prepared.conflicts,
                    prepared.returned_rows,
                    prepared.error,
                )
            }
        };
    let prepared_conflicts = Some(prepared_conflicts);
    let can_move_inserted_row = returning.is_none()
        && !schema
            .constraints
            .iter()
            .any(|constraint| matches!(constraint, Constraint::ForeignKey(_)));
    let has_referencing_foreign_keys = state.catalog.has_referencing_foreign_keys(schema.id);
    let mut returned_rows = Vec::new();
    let mut affected = 0;
    let mut affected_rows = BTreeSet::new();
    let mut inserted_rows = Vec::new();
    for (row_index, row) in rows.into_iter().enumerate() {
        match execute_insert_conflict(
            state,
            &schema,
            &row,
            conflict_arbiter.as_ref(),
            conflict_update.as_ref(),
            &affected_rows,
            has_referencing_foreign_keys,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            context,
            prepared_conflicts
                .as_ref()
                .and_then(|conflicts| conflicts[row_index].as_ref()),
        )? {
            InsertConflictOutcome::Skip => continue,
            InsertConflictOutcome::Update { row_id, row } => {
                affected += 1;
                affected_rows.insert(row_id);
                let prepared_returned = prepared_returned_rows
                    .as_ref()
                    .and_then(|rows| rows.get(row_index))
                    .and_then(|returned| returned.as_ref());
                if let Some(returned) = prepared_returned {
                    returned_rows.push(returned.clone());
                } else {
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
                continue;
            }
            InsertConflictOutcome::Insert => {}
        }
        if state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .has_visible_unique_conflict(
                &row,
                snapshot,
                xid,
                &state.transactions,
                None,
                None,
                None,
                None,
                context,
            )
        {
            return Err(PgError::create(
                SqlState::UniqueViolation,
                format!(
                    "duplicate key value violates unique constraint on {:?}",
                    schema.name
                ),
            ));
        }
        affected += 1;
        if !can_move_inserted_row {
            inserted_rows.push(row.clone());
        }
        let prepared_returned = prepared_returned_rows
            .as_ref()
            .and_then(|rows| rows.get(row_index))
            .and_then(|returned| returned.as_ref());
        if let Some(returned) = prepared_returned {
            returned_rows.push(returned.clone());
        }
        let row_id = insert_new_row(
            state,
            &schema,
            row,
            can_move_inserted_row,
            prepared_returned
                .is_none()
                .then_some(returning.as_ref())
                .flatten(),
            &mut returned_rows,
            xid,
            snapshot,
            context,
        )?;
        affected_rows.insert(row_id);
    }
    if let Some(error) = prepared_error {
        return Err(error);
    }
    for row in &inserted_rows {
        validate_row_foreign_keys(
            state,
            &schema,
            row,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            &inserted_rows,
        )?;
    }
    Ok(create_write_result(affected, returning, returned_rows))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_update(
    state: &mut DatabaseState,
    update: &ast::Update,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementExecutionContext,
    mutation_targets: Option<Vec<RequiredRowLock>>,
) -> Result<StatementResult> {
    let update_table = &update.table;
    let assignments = &update.assignments;
    let from = update.from.as_ref();
    let selection = update.selection.as_ref();
    let returning_items = update.returning.as_deref();
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
    let schema = require_mutation_table(state, &normalize_relation_name(table_name)?)?;
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
    let (assigned, assignments) = build_mutation_assignments(state, &schema, &scope, assignments)?;
    let prepared_updates = context.take_prepared_trigger_update(update).map(|updates| {
        updates
            .into_iter()
            .filter(|prepared| {
                !has_mutated_target_in_command(
                    state,
                    schema.id,
                    prepared.row_id,
                    prepared.version_xmin,
                    xid,
                    context.command_id,
                )
            })
            .collect::<Vec<_>>()
    });
    let prepared_targets =
        context.take_prepared_mutation_targets(update.span(), snapshot.commit_seq);
    let targets = match &prepared_updates {
        Some(prepared) => prepared
            .iter()
            .map(|prepared| {
                (
                    prepared.row_id,
                    prepared.version_xmin,
                    prepared.current.clone(),
                    prepared.bound_row.clone(),
                )
            })
            .collect(),
        None => match prepared_targets {
            Some(targets) => targets
                .into_iter()
                .filter(|target| {
                    !has_mutated_target_in_command(
                        state,
                        schema.id,
                        target.row_id,
                        target.version_xmin,
                        xid,
                        context.command_id,
                    )
                })
                .map(|target| {
                    (
                        target.row_id,
                        target.version_xmin,
                        target.current,
                        target.bound_row,
                    )
                })
                .collect(),
            None => {
                let source_rows = materialize_mutation_source_rows(
                    state,
                    from,
                    &scope,
                    schema.columns.len(),
                    xid,
                    snapshot,
                    context,
                )?;
                collect_mutation_targets(
                    state,
                    &schema,
                    selection,
                    &scope,
                    &source_rows,
                    xid,
                    snapshot,
                    context,
                    mutation_targets,
                )?
            }
        },
    };
    let updates_were_prepared = prepared_updates.is_some();
    let mut prepared_updates = prepared_updates.map(|updates| {
        updates
            .into_iter()
            .map(|prepared| (prepared.row_id, prepared.updated))
    });
    let mut affected = 0;
    let has_referencing_foreign_keys = state.catalog.has_referencing_foreign_keys(schema.id);
    let has_foreign_keys = schema
        .constraints
        .iter()
        .any(|constraint| matches!(constraint, Constraint::ForeignKey(_)));
    let can_move_updated_row =
        !has_referencing_foreign_keys && !has_foreign_keys && returning.is_none();
    let mut returned_rows = Vec::new();
    for (row_id, version_xmin, row, mut bound_row) in targets {
        let (old_row, mut updated) = if has_referencing_foreign_keys {
            (Some(row.clone()), row)
        } else {
            (None, row)
        };
        let updated = if let Some(prepared_updates) = &mut prepared_updates {
            let (prepared_row_id, updated) = prepared_updates
                .next()
                .expect("prepared trigger UPDATE retains every target row");
            assert_eq!(prepared_row_id, row_id);
            let Some(updated) = updated else {
                continue;
            };
            updated
        } else {
            let assignment_row = bound_row.as_deref().unwrap_or(&updated).to_vec();
            for assignment in &assignments {
                let target = schema.columns[assignment.index].data_type;
                updated[assignment.index] = if is_default_expression(assignment.expression) {
                    evaluate_column_default(&schema.columns[assignment.index], context)?
                } else if let Some(prepared) = &assignment.prepared {
                    prepared::evaluate_prepared_expression(
                        prepared,
                        &assignment_row,
                        &[],
                        context.deadline,
                    )?
                } else {
                    evaluate_mutation_assignment(
                        state,
                        assignment.expression,
                        target,
                        &scope,
                        &assignment_row,
                        xid,
                        snapshot,
                        context,
                    )?
                };
            }
            let Some(updated) = procedural::execute_before_row_triggers(
                state,
                &schema,
                procedural::TriggerEventKind::Update,
                updated,
                context,
            )?
            else {
                continue;
            };
            updated
        };
        affected += 1;
        if !updates_were_prepared {
            validate_not_null(&schema, &updated)?;
            validate_check_constraints(&schema, &updated, context)?;
        }
        if state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .has_visible_unique_conflict(
                &updated,
                snapshot,
                xid,
                &state.transactions,
                Some(row_id),
                schema.triggers.is_empty().then_some(&assigned),
                None,
                None,
                context,
            )
        {
            return Err(PgError::create(
                SqlState::UniqueViolation,
                format!(
                    "duplicate key value violates unique constraint on {:?}",
                    schema.name
                ),
            ));
        }
        if can_move_updated_row {
            state
                .tables
                .get_mut(&schema.id)
                .expect("catalog table must have storage")
                .append_updated_version(
                    row_id,
                    version_xmin,
                    xid,
                    context.command_id,
                    updated,
                    schema.triggers.is_empty().then_some(&assigned),
                );
            state.mark_table_touched(xid, schema.id);
            continue;
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
                schema.triggers.is_empty().then_some(&assigned),
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
            &[],
        )?;
        if has_referencing_foreign_keys {
            apply_referencing_foreign_key_actions(
                state,
                &schema,
                old_row
                    .as_ref()
                    .expect("referencing foreign keys retain the old row"),
                Some(&updated),
                xid,
                snapshot,
                deferred_constraints,
                defer_all,
                &mut BTreeSet::new(),
                context,
            )?;
        }
        if let Some(bound_row) = &mut bound_row {
            bound_row[..schema.columns.len()].clone_from_slice(&updated);
        }
        evaluate_returning_row(
            state,
            returning.as_ref(),
            bound_row.as_deref().unwrap_or(&updated),
            &mut returned_rows,
            xid,
            snapshot,
            context,
        )?;
    }
    assert!(prepared_updates.is_none_or(|mut rows| rows.next().is_none()));
    Ok(create_write_result(affected, returning, returned_rows))
}

pub(super) fn prepare_triggered_update_rows(
    state: &DatabaseState,
    update: &ast::Update,
    schema: &TableSchema,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<PreparedTriggerUpdate>> {
    let (index, prepared) = context.get_prepared_trigger_update(update);
    if let Some(rows) = prepared {
        return Ok(rows);
    }
    let alias = match &update.table.relation {
        ast::TableFactor::Table { alias, .. } => alias.as_ref().map(|alias| &alias.name),
        _ => unreachable!("lockable UPDATE targets a table"),
    };
    let from = match &update.from {
        None => &[][..],
        Some(ast::UpdateTableFromKind::AfterSet(from)) => from.as_slice(),
        Some(ast::UpdateTableFromKind::BeforeSet(_)) => {
            return reject_unsupported("UPDATE FROM before SET is not implemented");
        }
    };
    let scope = create_mutation_scope(state, schema, alias, from)?;
    let (_, assignments) = build_mutation_assignments(state, schema, &scope, &update.assignments)?;
    let targets = match context.get_prepared_mutation_targets(update.span(), snapshot.commit_seq) {
        Some(targets) => targets
            .into_iter()
            .filter(|target| {
                !has_mutated_target_in_command(
                    state,
                    schema.id,
                    target.row_id,
                    target.version_xmin,
                    xid,
                    context.command_id,
                )
            })
            .map(|target| {
                (
                    target.row_id,
                    target.version_xmin,
                    target.current,
                    target.bound_row,
                )
            })
            .collect(),
        None => {
            let source_rows = materialize_mutation_source_rows(
                state,
                from,
                &scope,
                schema.columns.len(),
                xid,
                snapshot,
                context,
            )?;
            collect_mutation_targets(
                state,
                schema,
                update.selection.as_ref(),
                &scope,
                &source_rows,
                xid,
                snapshot,
                context,
                None,
            )?
        }
    };
    let mut rows = Vec::new();
    let mut validation_table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .clone();
    for (row_id, version_xmin, current, bound_row) in targets {
        let mut updated = current.clone();
        let assignment_row = bound_row.as_deref().unwrap_or(&current);
        for assignment in &assignments {
            updated[assignment.index] = if is_default_expression(assignment.expression) {
                evaluate_column_default(&schema.columns[assignment.index], context)?
            } else if let Some(prepared) = &assignment.prepared {
                prepared::evaluate_prepared_expression(
                    prepared,
                    &assignment_row,
                    &[],
                    context.deadline,
                )?
            } else {
                evaluate_mutation_assignment(
                    state,
                    assignment.expression,
                    schema.columns[assignment.index].data_type,
                    &scope,
                    &assignment_row,
                    xid,
                    snapshot,
                    context,
                )?
            };
        }
        let updated = procedural::execute_before_row_triggers(
            state,
            schema,
            procedural::TriggerEventKind::Update,
            updated,
            context,
        )?;
        if let Some(updated) = &updated {
            validate_not_null(schema, updated)?;
            validate_check_constraints(schema, updated, context)?;
            if validation_table.has_visible_unique_conflict(
                updated,
                snapshot,
                xid,
                &state.transactions,
                Some(row_id),
                None,
                None,
                None,
                context,
            ) {
                return Err(PgError::create(
                    SqlState::UniqueViolation,
                    format!(
                        "duplicate key value violates unique constraint on {:?}",
                        schema.name
                    ),
                ));
            }
            validation_table.append_updated_version(
                row_id,
                version_xmin,
                xid,
                context.command_id,
                updated.clone(),
                None,
            );
        }
        rows.push(PreparedTriggerUpdate {
            row_id,
            version_xmin,
            current,
            bound_row,
            updated,
        });
    }
    context.set_prepared_trigger_update(index, update.clone(), rows.clone());
    Ok(rows)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_delete(
    state: &mut DatabaseState,
    delete: &ast::Delete,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementExecutionContext,
    mut mutation_targets: Option<Vec<RequiredRowLock>>,
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
    let schema = require_mutation_table(state, &normalize_relation_name(table_name)?)?;
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
    let has_referencing_foreign_keys = state.catalog.has_referencing_foreign_keys(schema.id);
    let prepared_targets =
        context.take_prepared_mutation_targets(delete.span(), snapshot.commit_seq);
    if using.is_empty() && returning.is_none() && !has_referencing_foreign_keys {
        if let Some(mutation_targets) = mutation_targets.take() {
            let targets = mutation_targets
                .into_iter()
                .filter(|required| required.key.table_id == schema.id)
                .collect::<Vec<_>>();
            let affected = targets.len() as u64;
            for required in targets {
                let candidate = required
                    .mutation_candidate
                    .expect("mutation target locks retain their selected version");
                state
                    .tables
                    .get_mut(&schema.id)
                    .expect("catalog table must have storage")
                    .mark_version_deleted(
                        required.key.row_id,
                        candidate.version_xmin,
                        xid,
                        context.command_id,
                    );
            }
            if affected != 0 {
                state.mark_table_touched(xid, schema.id);
            }
            return Ok(StatementResult::Affected(affected));
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
    let targets = match prepared_targets {
        Some(targets) => targets
            .into_iter()
            .filter(|target| {
                !has_mutated_target_in_command(
                    state,
                    schema.id,
                    target.row_id,
                    target.version_xmin,
                    xid,
                    context.command_id,
                )
            })
            .map(|target| {
                (
                    target.row_id,
                    target.version_xmin,
                    target.current,
                    target.bound_row,
                )
            })
            .collect(),
        None => collect_mutation_targets(
            state,
            &schema,
            delete.selection.as_ref(),
            &scope,
            &source_rows,
            xid,
            snapshot,
            context,
            mutation_targets,
        )?,
    };
    let affected = targets.len() as u64;
    let mut returned_rows = Vec::new();
    for (row_id, version_xmin, row, bound_row) in targets {
        if has_referencing_foreign_keys {
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
        }
        state
            .tables
            .get_mut(&schema.id)
            .expect("catalog table must have storage")
            .mark_version_deleted(row_id, version_xmin, xid, context.command_id);
        state.mark_table_touched(xid, schema.id);
        evaluate_returning_row(
            state,
            returning.as_ref(),
            bound_row.as_deref().unwrap_or(&row),
            &mut returned_rows,
            xid,
            snapshot,
            context,
        )?;
    }
    Ok(create_write_result(affected, returning, returned_rows))
}

type MutationTarget = (RowId, Xid, Vec<Value>, Option<Vec<Value>>);

fn has_mutated_target_in_command(
    state: &DatabaseState,
    table_id: TableId,
    row_id: RowId,
    version_xmin: Xid,
    xid: Xid,
    command_id: CommandId,
) -> bool {
    let table = state
        .tables
        .get(&table_id)
        .expect("catalog table must have storage");
    let (_, chain) = table
        .iterate_version_chains()
        .find(|(candidate, _)| *candidate == row_id)
        .expect("prepared mutation row must exist");
    assert!(
        chain
            .versions
            .iter()
            .any(|version| version.xmin == version_xmin),
        "prepared mutation version must exist"
    );
    chain.versions.iter().any(|version| {
        version.xmin == version_xmin
            && version.xmax == Some(xid)
            && version.xmax_command_id == Some(command_id)
    })
}

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
    let needs_bound_row = scope.columns.len() > schema.columns.len();
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
                let candidate_row = candidate
                    .row
                    .expect("row-consuming mutations retain their selected row");
                let bound_row = if needs_bound_row {
                    let mut row = source_row.clone();
                    row[..schema.columns.len()].clone_from_slice(&candidate_row);
                    Some(row)
                } else {
                    None
                };
                Ok((
                    required.key.row_id,
                    candidate.version_xmin,
                    candidate_row,
                    bound_row,
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
            return Ok(vec![(
                row_id,
                version.xmin,
                version.row.clone(),
                needs_bound_row.then_some(row),
            )]);
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
                    targets.push((
                        row_id,
                        version.xmin,
                        version.row.clone(),
                        needs_bound_row.then_some(row),
                    ));
                    break;
                }
            }
            Ok(targets)
        })
}
