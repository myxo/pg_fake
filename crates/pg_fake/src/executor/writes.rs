use super::*;
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
fn resolve_insert_column_indexes(
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
fn evaluate_insert_rows(
    state: &mut DatabaseState,
    insert: &ast::Insert,
    schema: &TableSchema,
    column_indexes: &[usize],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<Vec<Value>>> {
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
        validate_not_null(schema, &row)?;
        validate_check_constraints(schema, &row, context)?;
        Ok(row)
    };
    let Some(source) = &insert.source else {
        assert!(insert.columns.is_empty());
        let row = schema
            .columns
            .iter()
            .enumerate()
            .map(|(index, _)| evaluate_default(index))
            .collect::<Result<Vec<_>>>()?;
        validate_not_null(schema, &row)?;
        validate_check_constraints(schema, &row, context)?;
        return Ok(vec![row]);
    };
    if let ast::SetExpr::Values(values) = source.body.as_ref() {
        return values
            .rows
            .iter()
            .map(|expressions| build_row(expressions))
            .collect();
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
            for index in 0..schema.columns.len() {
                if !provided.contains(&index) {
                    row[index] = evaluate_default(index)?;
                }
            }
            for (((value, source_column), unknown), index) in values
                .into_iter()
                .zip(&source.columns)
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
            validate_not_null(schema, &row)?;
            validate_check_constraints(schema, &row, context)?;
            Ok(row)
        })
        .collect()
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
            prepared::evaluate_prepared_expression(prepared, &bound_row, &[])?
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
            Some(&update.assigned),
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
            Some(&update.assigned),
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
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementExecutionContext,
) -> Result<RowId> {
    let retained_row = (!can_move_row).then(|| row.clone());
    let row_id = state
        .tables
        .get_mut(&schema.id)
        .expect("catalog table must have storage")
        .insert(xid, context.command_id, row);
    assert!(matches!(
        state.row_locks.acquire(
            RowLockKey {
                table_id: schema.id,
                row_id,
            },
            xid,
            RowLockMode::Update,
        ),
        RowLockAttempt::Acquired
    ));
    state.mark_table_touched(xid, schema.id);
    if let Some(row) = retained_row {
        validate_row_foreign_keys(
            state,
            schema,
            &row,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
        )?;
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
    let schema = state.catalog.require_named_table(&table_name)?.clone();
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
    let rows = evaluate_insert_rows(
        state,
        insert,
        &schema,
        &column_indexes,
        xid,
        snapshot,
        context,
    )?;
    let can_move_inserted_row = returning.is_none()
        && !schema
            .constraints
            .iter()
            .any(|constraint| matches!(constraint, Constraint::ForeignKey(_)));
    let has_referencing_foreign_keys = state.catalog.has_referencing_foreign_keys(schema.id);
    let mut returned_rows = Vec::new();
    let mut affected = 0;
    let mut affected_rows = BTreeSet::new();
    for row in rows {
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
        )? {
            InsertConflictOutcome::Skip => continue,
            InsertConflictOutcome::Update { row_id, row } => {
                affected += 1;
                affected_rows.insert(row_id);
                evaluate_returning_row(
                    state,
                    returning.as_ref(),
                    &row,
                    &mut returned_rows,
                    xid,
                    snapshot,
                    context,
                )?;
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
        let row_id = insert_new_row(
            state,
            &schema,
            row,
            can_move_inserted_row,
            returning.as_ref(),
            &mut returned_rows,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            context,
        )?;
        affected_rows.insert(row_id);
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
    deferred_constraints: &BTreeSet<ConstraintId>,
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
        .require_named_table(&normalize_relation_name(table_name)?)?
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
    let (assigned, assignments) = build_mutation_assignments(state, &schema, &scope, assignments)?;
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
    let has_referencing_foreign_keys = state.catalog.has_referencing_foreign_keys(schema.id);
    let has_foreign_keys = schema
        .constraints
        .iter()
        .any(|constraint| matches!(constraint, Constraint::ForeignKey(_)));
    let can_move_updated_row =
        !has_referencing_foreign_keys && !has_foreign_keys && returning.is_none();
    let mut returned_rows = Vec::new();
    for (row_id, version_xmin, row, mut bound_row) in targets {
        let mut assigned_values = Vec::with_capacity(assignments.len());
        for assignment in &assignments {
            let target = schema.columns[assignment.index].data_type;
            let assignment_row = bound_row.as_deref().unwrap_or(&row);
            let value = if is_default_expression(assignment.expression) {
                evaluate_column_default(&schema.columns[assignment.index], context)?
            } else if let Some(prepared) = &assignment.prepared {
                prepared::evaluate_prepared_expression(prepared, assignment_row, &[])?
            } else {
                evaluate_mutation_assignment(
                    state,
                    assignment.expression,
                    target,
                    &scope,
                    assignment_row,
                    xid,
                    snapshot,
                    context,
                )?
            };
            assigned_values.push((assignment.index, value));
        }
        let (old_row, mut updated) = if has_referencing_foreign_keys {
            (Some(row.clone()), row)
        } else {
            (None, row)
        };
        for (index, value) in assigned_values {
            updated[index] = value;
        }
        validate_not_null(&schema, &updated)?;
        validate_check_constraints(&schema, &updated, context)?;
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
                Some(&assigned),
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
                    Some(&assigned),
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
                Some(&assigned),
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
    Ok(create_write_result(affected, returning, returned_rows))
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
    let schema = state
        .catalog
        .require_named_table(&normalize_relation_name(table_name)?)?
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
    let has_referencing_foreign_keys = state.catalog.has_referencing_foreign_keys(schema.id);
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
