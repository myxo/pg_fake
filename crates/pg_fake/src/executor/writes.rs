use super::*;

pub(super) fn insert_rows(
    state: &mut DatabaseState,
    insert: &sqlparser::ast::Insert,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    context: &ExecutionContext,
) -> Result<StatementResult> {
    let table_name = insert_table_name(&insert.table)?;
    let schema = state.catalog.table(&table_name)?.clone();
    if insert.returning.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "INSERT RETURNING is not implemented",
        ));
    }
    let column_indexes = if insert.columns.is_empty() {
        (0..schema.columns.len()).collect::<Vec<_>>()
    } else {
        insert
            .columns
            .iter()
            .map(|name| {
                let name = crate::executor::name(name)?;
                schema
                    .columns
                    .iter()
                    .position(|column| column.name == name)
                    .ok_or_else(|| {
                        PgError::new(
                            SqlState::UndefinedColumn,
                            format!("column {:?} does not exist", name),
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?
    };
    let provided = column_indexes.iter().copied().collect::<BTreeSet<_>>();
    let build_row = |expressions: &[Expr]| -> Result<Vec<Value>> {
        if expressions.len() != column_indexes.len() {
            return Err(PgError::new(
                SqlState::SyntaxError,
                "INSERT has wrong number of values",
            ));
        }
        let mut row = vec![Value::Null; schema.columns.len()];
        for (index, column) in schema.columns.iter().enumerate() {
            if !provided.contains(&index) {
                row[index] = column_default(column, context)?;
            }
        }
        let constants = constant_schema();
        for (expr, index) in expressions.iter().zip(&column_indexes) {
            row[*index] = if default_expression(expr) {
                column_default(&schema.columns[*index], context)?
            } else {
                expression_value(
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
        let SetExpr::Values(values) = source.body.as_ref() else {
            return Err(PgError::new(
                SqlState::FeatureNotSupported,
                "INSERT source is not implemented",
            ));
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
            .map(|column| column_default(column, context))
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
            .unique_conflict(&row, snapshot, xid, &state.transactions, None)
        {
            return Err(PgError::new(
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

pub(super) fn update_rows(
    state: &mut DatabaseState,
    update_table: &TableWithJoins,
    assignments: &[sqlparser::ast::Assignment],
    selection: Option<&Expr>,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    context: &ExecutionContext,
) -> Result<StatementResult> {
    if !update_table.joins.is_empty() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "UPDATE joins are not implemented",
        ));
    }
    let TableFactor::Table {
        name: table_name,
        args,
        ..
    } = &update_table.relation
    else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "UPDATE target is not implemented",
        ));
    };
    if args.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "UPDATE table functions are not implemented",
        ));
    }
    let schema = state.catalog.table(&name(table_name)?)?.clone();
    if let Some(selection) = selection {
        let base = expression_type(selection, RowScope::Table(&schema))?;
        if base != BaseType::Bool && !null_expression(selection) {
            return Err(PgError::new(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let mut assigned = BTreeSet::new();
    let assignments = assignments
        .iter()
        .map(|assignment| {
            let AssignmentTarget::ColumnName(column) = &assignment.target else {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "UPDATE tuple assignment is not implemented",
                ));
            };
            let column_name = name(column)?;
            let index = schema
                .columns
                .iter()
                .position(|definition| definition.name == column_name)
                .ok_or_else(|| {
                    PgError::new(
                        SqlState::UndefinedColumn,
                        format!("column {column_name:?} does not exist"),
                    )
                })?;
            if !assigned.insert(index) {
                return Err(PgError::new(
                    SqlState::SyntaxError,
                    "multiple assignments to the same column",
                ));
            }
            if !default_expression(&assignment.value)
                && !null_expression(&assignment.value)
                && unknown_string(&assignment.value).is_none()
                && !coercion::can_cast(
                    expression_type(&assignment.value, RowScope::Table(&schema))?,
                    schema.columns[index].data_type.base,
                    CastContext::Assignment,
                )
            {
                return Err(PgError::new(
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
        .rows()
        .try_fold(Vec::new(), |mut targets, (row_id, chain)| {
            let Some(version) = visible_version(chain, snapshot, xid, &state.transactions) else {
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
            updated[*index] = if default_expression(expression) {
                column_default(&schema.columns[*index], context)?
            } else {
                expression_value(expression, target, &schema, &row, context)?
            };
        }
        validate_not_null(&schema, &updated)?;
        validate_check_constraints(&schema, &updated, context)?;
        if state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .unique_conflict(&updated, snapshot, xid, &state.transactions, Some(row_id))
        {
            return Err(PgError::new(
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
            .update(row_id, version_xmin, xid, updated.clone());
        validate_row_foreign_keys(
            state,
            &schema,
            &updated,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
        )?;
        apply_parent_actions(
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

pub(super) fn delete_rows(
    state: &mut DatabaseState,
    delete: &Delete,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    context: &ExecutionContext,
) -> Result<StatementResult> {
    if !delete.tables.is_empty()
        || delete.using.is_some()
        || delete.returning.is_some()
        || !delete.order_by.is_empty()
        || delete.limit.is_some()
    {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "DELETE feature is not implemented",
        ));
    }
    let FromTable::WithFromKeyword(from) = &delete.from else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "DELETE without FROM is not implemented",
        ));
    };
    if from.len() != 1 || !from[0].joins.is_empty() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "DELETE joins are not implemented",
        ));
    }
    let TableFactor::Table {
        name: table_name,
        args,
        ..
    } = &from[0].relation
    else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "DELETE target is not implemented",
        ));
    };
    if args.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "DELETE table functions are not implemented",
        ));
    }
    let schema = state.catalog.table(&name(table_name)?)?.clone();
    if let Some(selection) = &delete.selection {
        let base = expression_type(selection, RowScope::Table(&schema))?;
        if base != BaseType::Bool && !null_expression(selection) {
            return Err(PgError::new(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let targets = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .rows()
        .try_fold(Vec::new(), |mut targets, (row_id, chain)| {
            let Some(version) = visible_version(chain, snapshot, xid, &state.transactions) else {
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
            .rows()
            .find_map(|(candidate, chain)| {
                (candidate == row_id).then(|| {
                    visible_version(chain, snapshot, xid, &state.transactions)
                        .map(|version| version.row.clone())
                })
            })
            .flatten()
            .expect("target row must remain visible");
        apply_parent_actions(
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
            .tombstone(row_id, version_xmin, xid);
    }
    Ok(StatementResult::Affected(affected))
}
