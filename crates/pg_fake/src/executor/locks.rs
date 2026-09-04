use super::*;
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn collect_required_cte_row_locks(
    state: &DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<RequiredRowLock>> {
    let ast::Statement::Query(query) = statement else {
        return Ok(Vec::new());
    };
    if query::has_zero_limit(query)
        && query.with.as_ref().is_none_or(|with| {
            with.cte_tables.iter().all(|cte| {
                !matches!(
                    cte.query.body.as_ref(),
                    ast::SetExpr::Insert(_) | ast::SetExpr::Update(_) | ast::SetExpr::Delete(_)
                )
            })
        })
    {
        return Ok(Vec::new());
    }
    let reachable = query::collect_reachable_cte_names(query);
    let mut locks = Vec::new();
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            if !reachable.contains(&normalize_identifier(&cte.alias.name)) {
                continue;
            }
            let statement = query::convert_query_to_statement(cte.query.as_ref().clone());
            locks.extend(collect_required_cte_row_locks(
                state, &statement, xid, snapshot, context,
            )?);
            locks.extend(collect_required_row_locks(
                state, &statement, xid, snapshot, context,
            )?);
        }
    }
    Ok(locks)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn collect_required_row_locks(
    state: &DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<RequiredRowLock>> {
    if let ast::Statement::Insert(insert) = statement {
        let mut locks = collect_insert_foreign_key_locks(state, insert, xid, snapshot, context)?;
        locks.extend(collect_insert_conflict_locks(state, insert, xid, context)?);
        return Ok(locks);
    }
    let target = match statement {
        ast::Statement::Update(update) => {
            let table = &update.table;
            if !table.joins.is_empty() {
                return Ok(Vec::new());
            }
            let ast::TableFactor::Table {
                name: table_name,
                alias,
                args: None,
                ..
            } = &table.relation
            else {
                return Ok(Vec::new());
            };
            (
                state
                    .catalog
                    .require_named_table(&normalize_relation_name(table_name)?)?,
                alias.as_ref().map(|alias| &alias.name),
                update
                    .from
                    .is_none()
                    .then_some(update.selection.as_ref())
                    .flatten(),
                RowLockMode::Update,
                update.from.is_none(),
                true,
            )
        }
        ast::Statement::Delete(delete) => {
            let ast::FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(Vec::new());
            };
            if from.len() != 1 || !from[0].joins.is_empty() {
                return Ok(Vec::new());
            }
            let ast::TableFactor::Table {
                name: table_name,
                alias,
                args: None,
                ..
            } = &from[0].relation
            else {
                return Ok(Vec::new());
            };
            let schema = state
                .catalog
                .require_named_table(&normalize_relation_name(table_name)?)?;
            (
                schema,
                alias.as_ref().map(|alias| &alias.name),
                delete
                    .using
                    .is_none()
                    .then_some(delete.selection.as_ref())
                    .flatten(),
                RowLockMode::Update,
                delete.using.is_none(),
                delete.returning.is_some() || state.catalog.has_referencing_foreign_keys(schema.id),
            )
        }
        ast::Statement::Query(query) => {
            let Some(mode) = query::resolve_select_lock_mode(query)? else {
                return Ok(Vec::new());
            };
            let ast::SetExpr::Select(select) = query.body.as_ref() else {
                return Ok(Vec::new());
            };
            if select.from.len() != 1 || !select.from[0].joins.is_empty() {
                return Ok(Vec::new());
            }
            let ast::TableFactor::Table {
                name: table_name,
                alias,
                args: None,
                ..
            } = &select.from[0].relation
            else {
                return Ok(Vec::new());
            };
            (
                state
                    .catalog
                    .require_named_table(&normalize_relation_name(table_name)?)?,
                alias.as_ref().map(|alias| &alias.name),
                select.selection.as_ref(),
                mode,
                false,
                false,
            )
        }
        _ => return Ok(Vec::new()),
    };
    let (schema, alias, selection, mode, retain_mutation_candidates, retain_mutation_row) = target;
    if let Some(selection) = selection {
        let base = infer_expression_type(selection, RowScope::Table(schema))?;
        if base != BaseType::Bool && !is_null_literal(selection) {
            return Ok(Vec::new());
        }
    }
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    if let Some((column, value)) =
        resolve_unique_point_lookup(table, schema, selection, RowScope::Table(schema), context)?
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
        check_concurrent_update(state, version, xid, snapshot)?;
        return Ok(vec![RequiredRowLock {
            key: RowLockKey {
                table_id: schema.id,
                row_id,
            },
            mode,
            mutation_candidate: retain_mutation_candidates.then(|| MutationCandidate {
                version_xmin: version.xmin,
                row: retain_mutation_row.then(|| version.row.clone()),
            }),
        }]);
    }
    let bound_scope = bind_target_scope(schema, alias);
    let prepared_selection = match selection {
        Some(selection) => prepared::bind_prepared_expression(selection, &bound_scope, &[])?
            .filter(|expression| expression.get_data_type() == BaseType::Bool),
        None => None,
    };
    table
        .iterate_version_chains()
        .try_fold(Vec::new(), |mut locks, (row_id, chain)| {
            let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions)
            else {
                return Ok(locks);
            };
            if let Some(selection) = selection {
                let value = if let Some(prepared_selection) = &prepared_selection {
                    prepared::evaluate_prepared_expression(prepared_selection, &version.row, &[])?
                } else {
                    evaluate(selection, RowScope::Table(schema), &version.row, context)?
                };
                match value {
                    Value::Bool(true) => {}
                    Value::Bool(false) | Value::Null => return Ok(locks),
                    _ => return Ok(locks),
                }
            }
            check_concurrent_update(state, version, xid, snapshot)?;
            locks.push(RequiredRowLock {
                key: RowLockKey {
                    table_id: schema.id,
                    row_id,
                },
                mode,
                mutation_candidate: retain_mutation_candidates.then(|| MutationCandidate {
                    version_xmin: version.xmin,
                    row: retain_mutation_row.then(|| version.row.clone()),
                }),
            });
            Ok(locks)
        })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn collect_insert_conflict_locks(
    state: &DatabaseState,
    insert: &ast::Insert,
    xid: Xid,
    context: &StatementExecutionContext,
) -> Result<Vec<RequiredRowLock>> {
    let schema = state
        .catalog
        .require_named_table(&resolve_insert_table_name(&insert.table)?)?;
    let Some(_) = writes::resolve_conflict_arbiter(schema, insert.on.as_ref())? else {
        return Ok(Vec::new());
    };
    let updates_conflict = matches!(
        insert.on,
        Some(ast::OnInsert::OnConflict(ast::OnConflict {
            action: ast::OnConflictAction::DoUpdate(_),
            ..
        }))
    );
    let values = insert.source.as_ref().and_then(|source| {
        if let ast::SetExpr::Values(values) = source.body.as_ref() {
            Some(values)
        } else {
            None
        }
    });
    let column_indexes = if insert.columns.is_empty() {
        (0..schema.columns.len()).collect::<Vec<_>>()
    } else {
        insert
            .columns
            .iter()
            .map(|column| {
                let name = normalize_unqualified_object_name(column)?;
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
            .collect::<Result<Vec<_>>>()?
    };
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    let mut locks = Vec::new();
    let mut needs_fallback = values.is_none() || updates_conflict;
    for expressions in values.into_iter().flat_map(|values| &values.rows) {
        if expressions.len() != column_indexes.len() {
            continue;
        }
        let mut row = Vec::with_capacity(schema.columns.len());
        let mut evaluable = true;
        for (index, column) in schema.columns.iter().enumerate() {
            let expression = column_indexes
                .iter()
                .position(|provided| *provided == index)
                .map(|position| &expressions[position]);
            let value = match expression {
                Some(expression) if !is_default_expression(expression) => {
                    let mut has_function = false;
                    let _ = ast::visit_expressions(expression, |nested| {
                        if matches!(nested, ast::Expr::Function(_)) {
                            has_function = true;
                            return std::ops::ControlFlow::Break(());
                        }
                        std::ops::ControlFlow::Continue(())
                    });
                    if has_function {
                        evaluable = false;
                        break;
                    }
                    evaluate_assignment_expression(
                        expression,
                        column.data_type,
                        &create_constant_expression_schema(),
                        &[],
                        context,
                    )?
                }
                Some(_) | None => {
                    if column.default_sequence.is_some()
                        || !matches!(column.default, None | Some(ast::Expr::Value(_)))
                    {
                        evaluable = false;
                        break;
                    }
                    evaluate_column_default(column, context)?
                }
            };
            row.push(value);
        }
        if !evaluable {
            needs_fallback = true;
            continue;
        }
        if let Some(row_id) =
            table.find_conflicting_row(&row, xid, &state.transactions, None, None, context)
        {
            locks.push(RequiredRowLock {
                key: RowLockKey {
                    table_id: schema.id,
                    row_id,
                },
                mode: RowLockMode::Update,
                mutation_candidate: None,
            });
        }
    }
    if needs_fallback {
        locks.clear();
        locks.extend(
            table
                .find_unique_candidate_rows(xid, &state.transactions, None, None, context)
                .into_iter()
                .map(|row_id| RequiredRowLock {
                    key: RowLockKey {
                        table_id: schema.id,
                        row_id,
                    },
                    mode: RowLockMode::Update,
                    mutation_candidate: None,
                }),
        );
    }
    Ok(locks)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn mutation_locks_cover_targets(statement: &ast::Statement) -> bool {
    match statement {
        ast::Statement::Update(update) => {
            update.from.is_none()
                && update.table.joins.is_empty()
                && matches!(
                    update.table.relation,
                    ast::TableFactor::Table { args: None, .. }
                )
        }
        ast::Statement::Delete(delete) => {
            delete.using.is_none()
                && matches!(
                    &delete.from,
                    ast::FromTable::WithFromKeyword(from)
                        if matches!(from.as_slice(), [table]
                            if table.joins.is_empty()
                                && matches!(table.relation, ast::TableFactor::Table { args: None, .. }))
                )
        }
        _ => false,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn check_concurrent_update(
    state: &DatabaseState,
    version: &crate::storage::RowVersion,
    xid: Xid,
    snapshot: &Snapshot,
) -> Result<()> {
    if version.xmax.is_some_and(|xmax| {
        xmax != xid
            && matches!(
                state.transactions.get_status(xmax),
                Some(TransactionStatus::Committed(commit_seq)) if commit_seq > snapshot.commit_seq
            )
    }) {
        return Err(PgError::create(
            SqlState::SerializationFailure,
            "could not serialize access due to concurrent update",
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn resolve_unique_point_lookup(
    table: &Table,
    schema: &TableSchema,
    selection: Option<&ast::Expr>,
    scope: RowScope<'_>,
    context: &StatementExecutionContext,
) -> Result<Option<(usize, Value)>> {
    let Some(ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::Eq,
        right,
    }) = selection
    else {
        return Ok(None);
    };
    let (column, value) = match (left.as_ref(), right.as_ref()) {
        (ast::Expr::Identifier(column), value) if is_point_lookup_value(value) => {
            (std::slice::from_ref(column), value)
        }
        (value, ast::Expr::Identifier(column)) if is_point_lookup_value(value) => {
            (std::slice::from_ref(column), value)
        }
        (ast::Expr::CompoundIdentifier(column), value) if is_point_lookup_value(value) => {
            (column.as_slice(), value)
        }
        (value, ast::Expr::CompoundIdentifier(column)) if is_point_lookup_value(value) => {
            (column.as_slice(), value)
        }
        _ => return Ok(None),
    };
    let Ok((column, _)) = scope.resolve_column(column) else {
        return Ok(None);
    };
    if column >= schema.columns.len()
        || !table.has_unique_index(&[column])
        || resolve_operator_type(left, right, scope)? != schema.columns[column].data_type.base
    {
        return Ok(None);
    }
    let value = evaluate_and_coerce(
        value,
        schema.columns[column].data_type.base,
        CastContext::Implicit,
        scope,
        &[],
        context,
    )?;
    Ok(Some((column, value)))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_point_lookup_value(expr: &ast::Expr) -> bool {
    matches!(expr, ast::Expr::Value(_))
        || matches!(
            expr,
            ast::Expr::Cast {
                kind: ast::CastKind::Cast | ast::CastKind::DoubleColon,
                expr,
                format: None,
                ..
            } if matches!(expr.as_ref(), ast::Expr::Value(_))
        )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn collect_insert_foreign_key_locks(
    state: &DatabaseState,
    insert: &ast::Insert,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<RequiredRowLock>> {
    let schema = state
        .catalog
        .require_named_table(&resolve_insert_table_name(&insert.table)?)?;
    if !schema
        .constraints
        .iter()
        .any(|constraint| matches!(constraint, crate::catalog::Constraint::ForeignKey(_)))
    {
        return Ok(Vec::new());
    }
    let Some(source) = &insert.source else {
        return Ok(Vec::new());
    };
    let ast::SetExpr::Values(values) = source.body.as_ref() else {
        return Ok(Vec::new());
    };
    let column_indexes = if insert.columns.is_empty() {
        (0..schema.columns.len()).collect::<Vec<_>>()
    } else {
        insert
            .columns
            .iter()
            .map(|column| {
                let column_name = normalize_unqualified_object_name(column)?;
                schema
                    .columns
                    .iter()
                    .position(|definition| definition.name == column_name)
                    .ok_or_else(|| {
                        PgError::create(
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
        for constraint in &schema.constraints {
            let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
                continue;
            };
            let local = resolve_foreign_key_column_indexes(schema, &foreign_key.columns)?;
            let mut key = Vec::new();
            for index in local {
                let Some(position) = column_indexes
                    .iter()
                    .position(|provided| *provided == index)
                else {
                    key.clear();
                    break;
                };
                let expression = &expressions[position];
                let mut has_side_effect = false;
                let _ = ast::visit_expressions(expression, |nested| {
                    if let ast::Expr::Function(function) = nested
                        && normalize_unqualified_object_name(&function.name).is_ok_and(|name| {
                            matches!(
                                name.as_str(),
                                "gen_random_uuid" | "uuidv4" | "uuidv7" | "nextval" | "setval"
                            )
                        })
                    {
                        has_side_effect = true;
                        return std::ops::ControlFlow::Break(());
                    }
                    std::ops::ControlFlow::Continue(())
                });
                if is_default_expression(expression) || has_side_effect {
                    key.clear();
                    break;
                }
                key.push(evaluate_assignment_expression(
                    expression,
                    schema.columns[index].data_type,
                    &create_constant_expression_schema(),
                    &[],
                    context,
                )?);
            }
            if key.len() != foreign_key.columns.len() {
                continue;
            }
            if key.iter().any(Value::is_null) {
                continue;
            }
            let foreign_schema = state
                .catalog
                .require_table_by_id(foreign_key.foreign_table_id)?;
            let referred = if foreign_key.referred_columns.is_empty() {
                foreign_schema
                    .constraints
                    .iter()
                    .find_map(|constraint| match constraint {
                        crate::catalog::Constraint::PrimaryKey { columns, .. } => {
                            Some(columns.clone())
                        }
                        _ => None,
                    })
                    .expect("foreign key definition was validated")
            } else {
                foreign_key.referred_columns.clone()
            };
            let referred = resolve_foreign_key_column_indexes(foreign_schema, &referred)?;
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
                    mutation_candidate: None,
                });
            }
        }
    }
    Ok(locks)
}
