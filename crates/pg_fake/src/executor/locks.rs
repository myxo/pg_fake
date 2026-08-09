use super::*;

pub(crate) fn required_row_locks(
    state: &DatabaseState,
    statement: &Statement,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
) -> Result<Vec<RequiredRowLock>> {
    if let Statement::Insert(insert) = statement {
        return required_insert_foreign_key_locks(state, insert, xid, snapshot, context);
    }
    let (schema, selection, mode) = match statement {
        Statement::Update(update) => {
            let table = &update.table;
            if !table.joins.is_empty() {
                return Ok(Vec::new());
            }
            let TableFactor::Table {
                name: table_name,
                args: None,
                ..
            } = &table.relation
            else {
                return Ok(Vec::new());
            };
            (
                state.catalog.table(&name(table_name)?)?,
                update.selection.as_ref(),
                RowLockMode::Update,
            )
        }
        Statement::Delete(delete) => {
            let FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(Vec::new());
            };
            if from.len() != 1 || !from[0].joins.is_empty() {
                return Ok(Vec::new());
            }
            let TableFactor::Table {
                name: table_name,
                args: None,
                ..
            } = &from[0].relation
            else {
                return Ok(Vec::new());
            };
            (
                state.catalog.table(&name(table_name)?)?,
                delete.selection.as_ref(),
                RowLockMode::Update,
            )
        }
        Statement::Query(query) => {
            let Some(mode) = query::select_lock_mode(query)? else {
                return Ok(Vec::new());
            };
            let SetExpr::Select(select) = query.body.as_ref() else {
                return Ok(Vec::new());
            };
            if select.from.len() != 1 || !select.from[0].joins.is_empty() {
                return Ok(Vec::new());
            }
            let TableFactor::Table {
                name: table_name,
                args: None,
                ..
            } = &select.from[0].relation
            else {
                return Ok(Vec::new());
            };
            (
                state.catalog.table(&name(table_name)?)?,
                select.selection.as_ref(),
                mode,
            )
        }
        _ => return Ok(Vec::new()),
    };
    if let Some(selection) = selection {
        let base = expression_type(selection, RowScope::Table(schema))?;
        if base != BaseType::Bool && !null_expression(selection) {
            return Ok(Vec::new());
        }
    }
    state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .rows()
        .try_fold(Vec::new(), |mut locks, (row_id, chain)| {
            let Some(version) = visible_version(chain, snapshot, xid, &state.transactions) else {
                return Ok(locks);
            };
            if let Some(selection) = selection {
                match evaluate(selection, RowScope::Table(schema), &version.row, context)? {
                    Value::Bool(true) => {}
                    Value::Bool(false) | Value::Null => return Ok(locks),
                    _ => return Ok(locks),
                }
            }
            if version.xmax.is_some_and(|xmax| {
                xmax != xid
                    && matches!(
                        state.transactions.status(xmax),
                        Some(TransactionStatus::Committed(commit_seq))
                            if commit_seq > snapshot.commit_seq
                    )
            }) {
                return Err(PgError::new(
                    SqlState::SerializationFailure,
                    "could not serialize access due to concurrent update",
                ));
            }
            locks.push(RequiredRowLock {
                key: RowLockKey {
                    table_id: schema.id,
                    row_id,
                },
                mode,
            });
            Ok(locks)
        })
}

fn required_insert_foreign_key_locks(
    state: &DatabaseState,
    insert: &sqlparser::ast::Insert,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
) -> Result<Vec<RequiredRowLock>> {
    let schema = state.catalog.table(&insert_table_name(&insert.table)?)?;
    let Some(source) = &insert.source else {
        return Ok(Vec::new());
    };
    let SetExpr::Values(values) = source.body.as_ref() else {
        return Ok(Vec::new());
    };
    let column_indexes = if insert.columns.is_empty() {
        (0..schema.columns.len()).collect::<Vec<_>>()
    } else {
        insert
            .columns
            .iter()
            .map(|column| {
                let column_name = name(column)?;
                schema
                    .columns
                    .iter()
                    .position(|definition| definition.name == column_name)
                    .ok_or_else(|| {
                        PgError::new(
                            SqlState::UndefinedColumn,
                            format!("column {:?} does not exist", column),
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?
    };
    let mut locks = Vec::new();
    for expressions in &values.rows {
        if expressions.len() != column_indexes.len() {
            continue;
        }
        let mut row = schema
            .columns
            .iter()
            .map(|column| column_default(column, context))
            .collect::<Result<Vec<_>>>()?;
        for (expression, index) in expressions.iter().zip(&column_indexes) {
            row[*index] = if default_expression(expression) {
                column_default(&schema.columns[*index], context)?
            } else {
                expression_value(
                    expression,
                    schema.columns[*index].data_type,
                    &constant_schema(),
                    &[],
                    context,
                )?
            };
        }
        for constraint in &schema.constraints {
            let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
                continue;
            };
            let local = foreign_key_column_indexes(schema, &foreign_key.columns)?;
            let key = local
                .iter()
                .map(|index| row[*index].clone())
                .collect::<Vec<_>>();
            if key.iter().any(Value::is_null) {
                continue;
            }
            let foreign_schema = state.catalog.table(&foreign_key.foreign_table)?;
            let referred = if foreign_key.referred_columns.is_empty() {
                foreign_schema
                    .constraints
                    .iter()
                    .find_map(|constraint| match constraint {
                        crate::catalog::Constraint::PrimaryKey(columns) => Some(columns.clone()),
                        _ => None,
                    })
                    .expect("foreign key definition was validated")
            } else {
                foreign_key.referred_columns.clone()
            };
            let referred = foreign_key_column_indexes(foreign_schema, &referred)?;
            let table = state
                .tables
                .get(&foreign_schema.id)
                .expect("catalog table must have storage");
            if let Some(row_id) =
                table.find_unique_row(&referred, &key, snapshot, xid, &state.transactions)
            {
                locks.push(RequiredRowLock {
                    key: RowLockKey {
                        table_id: foreign_schema.id,
                        row_id,
                    },
                    mode: RowLockMode::Share,
                });
            }
        }
    }
    Ok(locks)
}
