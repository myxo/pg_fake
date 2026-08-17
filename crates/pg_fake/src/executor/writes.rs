use super::*;
use sqlparser::ast;

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
    if insert.returning.is_some() {
        return reject_unsupported("INSERT RETURNING is not implemented");
    }
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
        let ast::SetExpr::Values(values) = source.body.as_ref() else {
            return reject_unsupported("INSERT source is not implemented");
        };
        values
            .rows
            .iter()
            .map(|expressions| build_row(expressions))
            .collect::<Result<Vec<_>>>()?
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
            .insert(xid, row.clone());
        validate_row_foreign_keys(
            state,
            &schema,
            &row,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
        )?;
    }
    Ok(StatementResult::Affected(affected))
}

pub(super) fn execute_update(
    state: &mut DatabaseState,
    update_table: &ast::TableWithJoins,
    assignments: &[ast::Assignment],
    selection: Option<&ast::Expr>,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    if !update_table.joins.is_empty() {
        return reject_unsupported("UPDATE joins are not implemented");
    }
    let ast::TableFactor::Table {
        name: table_name,
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
    if let Some(selection) = selection {
        let base = infer_expression_type(selection, RowScope::Table(&schema))?;
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
                    infer_expression_type(&assignment.value, RowScope::Table(&schema))?,
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
    let targets = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .iterate_version_chains()
        .try_fold(Vec::new(), |mut targets, (row_id, chain)| {
            let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions)
            else {
                return Ok(targets);
            };
            if let Some(selection) = selection {
                match evaluate(selection, RowScope::Table(&schema), &version.row, context)? {
                    Value::Bool(true) => {}
                    Value::Bool(false) | Value::Null => return Ok(targets),
                    _ => unreachable!("WHERE expression was type-checked"),
                }
            }
            targets.push((row_id, version.xmin, version.row.clone()));
            Ok(targets)
        })?;
    let affected = targets.len() as u64;
    for (row_id, version_xmin, row) in targets {
        let mut updated = row.clone();
        for (index, expression) in &assignments {
            let target = schema.columns[*index].data_type;
            updated[*index] = if is_default_expression(expression) {
                evaluate_column_default(&schema.columns[*index], context)?
            } else {
                evaluate_assignment_expression(expression, target, &schema, &row, context)?
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
            .append_updated_version(row_id, version_xmin, xid, updated.clone());
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
    }
    Ok(StatementResult::Affected(affected))
}

pub(super) fn execute_delete(
    state: &mut DatabaseState,
    delete: &ast::Delete,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    if !delete.tables.is_empty()
        || delete.using.is_some()
        || delete.returning.is_some()
        || !delete.order_by.is_empty()
        || delete.limit.is_some()
    {
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
    if let Some(selection) = &delete.selection {
        let base = infer_expression_type(selection, RowScope::Table(&schema))?;
        if base != BaseType::Bool && !is_null_literal(selection) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let targets = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .iterate_version_chains()
        .try_fold(Vec::new(), |mut targets, (row_id, chain)| {
            let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions)
            else {
                return Ok(targets);
            };
            if let Some(selection) = &delete.selection {
                match evaluate(selection, RowScope::Table(&schema), &version.row, context)? {
                    Value::Bool(true) => {}
                    Value::Bool(false) | Value::Null => return Ok(targets),
                    _ => unreachable!("WHERE expression was type-checked"),
                }
            }
            targets.push((row_id, version.xmin));
            Ok(targets)
        })?;
    let affected = targets.len() as u64;
    for (row_id, version_xmin) in targets {
        let row = state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .iterate_version_chains()
            .find_map(|(candidate, chain)| {
                (candidate == row_id).then(|| {
                    find_visible_version(chain, snapshot, xid, &state.transactions)
                        .map(|version| version.row.clone())
                })
            })
            .flatten()
            .expect("target row must remain visible");
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
            .mark_version_deleted(row_id, version_xmin, xid);
    }
    Ok(StatementResult::Affected(affected))
}
