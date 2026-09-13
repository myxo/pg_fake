use super::{
    conflicts::{build_conflict_update_plan, prepare_conflict_update, resolve_conflict_arbiter},
    returning::{ReturningPlan, build_returning_plan, evaluate_returning_row},
};
use crate::executor::{
    DatabaseState, PreparedConflictUpdate, PreparedInsert, StatementContext,
    column_defaults::{evaluate_column_default, is_default_expression},
    expressions::{create_constant_expression_schema, evaluate_assignment_expression},
    prepared, procedural, query, resolve_insert_table_name,
    row_constraints::{validate_check_constraints, validate_not_null},
    scope::{bind_target_scope, identify_unknown_query_columns},
};
use crate::{
    catalog::{IdentityKind, TableSchema},
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState},
    storage::Table,
    txn::{RowLockKey, RowLockMode, Snapshot, Xid},
    value::{BaseType, Value},
};
use sqlparser::ast;
use std::{collections::BTreeSet, sync::Arc};

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_insert_rows(
    state: &DatabaseState,
    insert: &ast::Insert,
    schema: &TableSchema,
    column_indexes: &[usize],
    returning: Option<&ReturningPlan<'_>>,
    resume: Option<&PreparedInsert>,
    stop_at_blocking_conflict: bool,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<PreparedInsert> {
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
        for (index, value) in row.iter_mut().enumerate() {
            if !provided.contains(&index) {
                *value = evaluate_default(index)?;
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
    let mut validation_table = if insert.on.is_some() || stop_at_blocking_conflict {
        state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .clone()
    } else {
        Table::create(schema.clone())
    };
    let mut affected_rows = BTreeSet::new();
    for (prior_insert, prepared) in if stop_at_blocking_conflict {
        context.get_prior_prepared_inserts(insert)
    } else {
        Default::default()
    } {
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
                        RowLockMode::NoKeyUpdate,
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
        let prepared_conflict = prepare_conflict_update(
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
        return Ok(PreparedInsert {
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
                        return Ok(PreparedInsert {
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
                    return Ok(PreparedInsert {
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
        return Ok(PreparedInsert {
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
                    return Ok(PreparedInsert {
                        source_state: Some(source_state.clone()),
                        source_snapshot: Some(*source_snapshot),
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
                return Ok(PreparedInsert {
                    source_state: Some(source_state.clone()),
                    source_snapshot: Some(*source_snapshot),
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
    let streamed = query::stream_query_rows(
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
                    for (index, value) in row.iter_mut().enumerate() {
                        if !provided.contains(&index) {
                            *value = evaluate_default(index)?;
                        }
                    }
                    for (((value, source_column), unknown), index) in values
                        .into_iter()
                        .zip(columns)
                        .zip(&unknown_columns)
                        .zip(column_indexes)
                    {
                        row[*index] = if *unknown {
                            match value {
                                Value::Text(text) => coercion::coerce_unknown(
                                    &text,
                                    schema.columns[*index].data_type,
                                    CastContext::Assignment,
                                    &context.timezone,
                                )?,
                                Value::Null => Value::Null,
                                _ => unreachable!("unknown literals evaluate to text or null"),
                            }
                        } else {
                            let source_type = BaseType::resolve_oid(source_column.type_oid)
                                .expect("query columns use supported PostgreSQL types");
                            coercion::coerce(
                                value,
                                source_type,
                                schema.columns[*index].data_type,
                                CastContext::Assignment,
                                &context.timezone,
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
            return Ok(PreparedInsert {
                source_state: Some(source_state.clone()),
                source_snapshot: Some(*source_snapshot),
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
                Some(error) => Ok(PreparedInsert {
                    source_state: Some(source_state.clone()),
                    source_snapshot: Some(*source_snapshot),
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
            return Ok(PreparedInsert {
                source_state: Some(source_state.clone()),
                source_snapshot: Some(*source_snapshot),
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
    let Some(query::QueryStreamState::Materialized { result: source, .. }) = source_query.as_ref()
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
            for (index, value) in row.iter_mut().enumerate() {
                if !provided.contains(&index) {
                    *value = evaluate_default(index)?;
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
                    match value {
                        Value::Text(text) => coercion::coerce_unknown(
                            &text,
                            schema.columns[*index].data_type,
                            CastContext::Assignment,
                            &context.timezone,
                        )?,
                        Value::Null => Value::Null,
                        _ => unreachable!("unknown literals evaluate to text or null"),
                    }
                } else {
                    let source_type = BaseType::resolve_oid(source_column.type_oid)
                        .expect("query columns use supported PostgreSQL types");
                    coercion::coerce(
                        value,
                        source_type,
                        schema.columns[*index].data_type,
                        CastContext::Assignment,
                        &context.timezone,
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
                    return Ok(PreparedInsert {
                        source_state: Some(source_state.clone()),
                        source_snapshot: Some(*source_snapshot),
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
                return Ok(PreparedInsert {
                    source_state: Some(source_state.clone()),
                    source_snapshot: Some(*source_snapshot),
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
    Ok(PreparedInsert {
        source_state: Some(source_state),
        source_snapshot: Some(*source_snapshot),
        source_query,
        source_rows: streamed_source_rows,
        rows: streamed_rows,
        conflicts: streamed_conflicts,
        returned_rows: prepared_returning(streamed_returned_rows),
        error: None,
        complete: !stopped.get(),
    })
}

pub(in crate::executor) fn prepare_insert_rows(
    state: &DatabaseState,
    insert: &ast::Insert,
    schema: &TableSchema,
    column_indexes: &[usize],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<PreparedInsert> {
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
    let resume = context.get_prepared_insert(insert);
    let prepared = match resume {
        Some(prepared) if prepared.complete => prepared,
        resume => evaluate_insert_rows(
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
    context.set_prepared_insert(insert, prepared.clone());
    if !prepared.complete {
        context.request_row_lock_recheck();
    } else if let Some(error) = prepared.error.clone() {
        return Err(error);
    }
    Ok(prepared)
}
