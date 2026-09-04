use super::*;
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn convert_referential_action(
    action: Option<ast::ReferentialAction>,
) -> ForeignKeyAction {
    match action.unwrap_or(ast::ReferentialAction::NoAction) {
        ast::ReferentialAction::NoAction => ForeignKeyAction::NoAction,
        ast::ReferentialAction::Restrict => ForeignKeyAction::Restrict,
        ast::ReferentialAction::Cascade => ForeignKeyAction::Cascade,
        ast::ReferentialAction::SetNull => ForeignKeyAction::SetNull,
        ast::ReferentialAction::SetDefault => ForeignKeyAction::SetDefault,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn resolve_foreign_key_name(name: Option<&ast::Ident>, default: String) -> String {
    let name = name.map(normalize_identifier).unwrap_or_default();
    if name.is_empty() { default } else { name }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_foreign_key_deferred(
    foreign_key: &ForeignKey,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
) -> bool {
    foreign_key.deferrable
        && (foreign_key.initially_deferred
            || defer_all
            || deferred_constraints.contains(&foreign_key.id))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn contains_deferred_foreign_keys(
    state: &DatabaseState,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
) -> bool {
    state
        .catalog
        .contains_deferred_foreign_keys(deferred_constraints, defer_all)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn resolve_foreign_key_column_indexes(
    schema: &TableSchema,
    columns: &[String],
) -> Result<Vec<usize>> {
    columns
        .iter()
        .map(|name| {
            schema
                .columns
                .iter()
                .position(|column| column.name == *name)
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::UndefinedColumn,
                        format!("column {name:?} does not exist"),
                    )
                })
        })
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn compare_foreign_key_keys(left: &[Value], right: &[Value]) -> Result<bool> {
    left.iter()
        .zip(right)
        .try_fold(true, |matches, (left, right)| {
            if !matches {
                return Ok(false);
            }
            Ok(matches!(
                evaluate_comparison(&ast::BinaryOperator::Eq, left, right)?,
                Value::Bool(true)
            ))
        })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn validate_foreign_key_definitions(
    catalog: &Catalog,
    schema: &TableSchema,
) -> Result<()> {
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
            continue;
        };
        if foreign_key.initially_deferred && !foreign_key.deferrable {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "constraint cannot be initially deferred because it is not deferrable",
            ));
        }
        let referred = if foreign_key.foreign_table.name == schema.name
            && foreign_key
                .foreign_table
                .schema
                .as_deref()
                .is_none_or(|name| {
                    catalog
                        .require_schema(name)
                        .is_ok_and(|candidate| candidate.id == schema.schema_id)
                }) {
            schema
        } else {
            catalog.require_named_table(&foreign_key.foreign_table)?
        };
        let local_is_temporary = matches!(schema.persistence, TablePersistence::Temporary { .. });
        let referred_is_temporary =
            matches!(referred.persistence, TablePersistence::Temporary { .. });
        if local_is_temporary != referred_is_temporary {
            return Err(PgError::create(
                SqlState::InvalidTableDefinition,
                "foreign keys between permanent and temporary tables are not implemented",
            ));
        }
        let referred_columns = if foreign_key.referred_columns.is_empty() {
            referred
                .constraints
                .iter()
                .find_map(|constraint| match constraint {
                    crate::catalog::Constraint::PrimaryKey { columns, .. } => Some(columns.clone()),
                    _ => None,
                })
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::InvalidColumnReference,
                        "there is no primary key for referenced table",
                    )
                })?
        } else {
            foreign_key.referred_columns.clone()
        };
        let local_indexes = resolve_foreign_key_column_indexes(schema, &foreign_key.columns)?;
        let referred_indexes = resolve_foreign_key_column_indexes(referred, &referred_columns)?;
        if local_indexes.len() != referred_indexes.len() {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "number of referencing and referenced columns for foreign key disagree",
            ));
        }
        if !referred.constraints.iter().any(|constraint| {
            matches!(constraint,
                crate::catalog::Constraint::PrimaryKey { columns, .. }
                | crate::catalog::Constraint::Unique { columns, .. }
                    if *columns == referred_columns
            )
        }) {
            return Err(PgError::create(
                SqlState::InvalidColumnReference,
                "there is no unique constraint matching given keys for referenced table",
            ));
        }
        for (local, referred_index) in local_indexes.iter().zip(referred_indexes) {
            if schema.columns[*local].data_type.base
                != referred.columns[referred_index].data_type.base
            {
                return Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "foreign key constraint cannot be implemented",
                ));
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn validate_row_foreign_keys(
    state: &DatabaseState,
    schema: &TableSchema,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
) -> Result<()> {
    let snapshot = snapshot.include_current_command();
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
            continue;
        };
        if is_foreign_key_deferred(foreign_key, deferred_constraints, defer_all) {
            continue;
        }
        let local_indexes = resolve_foreign_key_column_indexes(schema, &foreign_key.columns)?;
        let key = local_indexes
            .iter()
            .map(|index| row[*index].clone())
            .collect::<Vec<_>>();
        if key.iter().any(Value::is_null) {
            if foreign_key.match_kind == Some(ast::ConstraintReferenceMatchKind::Full)
                && !key.iter().all(Value::is_null)
            {
                return Err(PgError::create(
                    SqlState::ForeignKeyViolation,
                    format!(
                        "insert or update on table {:?} violates foreign key constraint {:?}",
                        schema.name, foreign_key.name
                    ),
                ));
            }
            continue;
        }
        let foreign_schema = state
            .catalog
            .require_table_by_id(foreign_key.foreign_table_id)?;
        let referred_columns = if foreign_key.referred_columns.is_empty() {
            foreign_schema
                .constraints
                .iter()
                .find_map(|constraint| match constraint {
                    crate::catalog::Constraint::PrimaryKey { columns, .. } => Some(columns.clone()),
                    _ => None,
                })
                .expect("foreign key definition was validated")
        } else {
            foreign_key.referred_columns.clone()
        };
        let referred_indexes =
            resolve_foreign_key_column_indexes(foreign_schema, &referred_columns)?;
        let found = state
            .tables
            .get(&foreign_schema.id)
            .expect("catalog table must have storage")
            .find_unique_row(&referred_indexes, &key, &snapshot, xid, &state.transactions)
            .is_some();
        if !found {
            return Err(PgError::create(
                SqlState::ForeignKeyViolation,
                format!(
                    "insert or update on table {:?} violates foreign key constraint {:?}",
                    schema.name, foreign_key.name
                ),
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn validate_deferred_foreign_keys(state: &DatabaseState, xid: Xid) -> Result<()> {
    let snapshot = Snapshot::create(&state.transactions);
    for schema in state.catalog.iterate_tables() {
        let mut schema = schema.clone();
        for constraint in &mut schema.constraints {
            if let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint {
                foreign_key.initially_deferred = false;
            }
        }
        let table = state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage");
        for (_, chain) in table.iterate_version_chains() {
            if let Some(version) = find_visible_version(chain, &snapshot, xid, &state.transactions)
            {
                validate_row_foreign_keys(
                    state,
                    &schema,
                    &version.row,
                    xid,
                    &snapshot,
                    &BTreeSet::new(),
                    false,
                )?;
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn apply_referencing_foreign_key_actions(
    state: &mut DatabaseState,
    parent_schema: &TableSchema,
    old_row: &[Value],
    new_row: Option<&[Value]>,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    visited: &mut BTreeSet<(TableId, RowId)>,
    context: &StatementExecutionContext,
) -> Result<()> {
    let snapshot = snapshot.include_current_command();
    let foreign_keys = state.catalog.referencing_foreign_keys(parent_schema.id);
    for (child_schema, foreign_key) in foreign_keys {
        let referred_columns = if foreign_key.referred_columns.is_empty() {
            parent_schema
                .constraints
                .iter()
                .find_map(|constraint| match constraint {
                    crate::catalog::Constraint::PrimaryKey { columns, .. } => Some(columns.clone()),
                    _ => None,
                })
                .expect("foreign key definition was validated")
        } else {
            foreign_key.referred_columns.clone()
        };
        let parent_indexes = resolve_foreign_key_column_indexes(parent_schema, &referred_columns)?;
        let old_key = parent_indexes
            .iter()
            .map(|index| old_row[*index].clone())
            .collect::<Vec<_>>();
        let new_key = new_row.map(|row| {
            parent_indexes
                .iter()
                .map(|index| row[*index].clone())
                .collect::<Vec<_>>()
        });
        if new_key.as_ref().is_some_and(|key| {
            compare_foreign_key_keys(&old_key, key)
                .expect("matching compatible foreign key values must work")
        }) {
            continue;
        }
        let child_indexes =
            resolve_foreign_key_column_indexes(&child_schema, &foreign_key.columns)?;
        let children = state
            .tables
            .get(&child_schema.id)
            .expect("catalog table must have storage")
            .iterate_version_chains()
            .try_fold(Vec::new(), |mut children, (row_id, chain)| {
                let Some(version) =
                    find_visible_version(chain, &snapshot, xid, &state.transactions)
                else {
                    return Ok(children);
                };
                if version.xmax == Some(xid) && version.xmax_command_id == Some(context.command_id)
                {
                    return Ok(children);
                }
                let key = child_indexes
                    .iter()
                    .map(|index| version.row[*index].clone())
                    .collect::<Vec<_>>();
                if !key.iter().any(Value::is_null) && compare_foreign_key_keys(&key, &old_key)? {
                    children.push((row_id, version.xmin, version.row.clone()));
                }
                Ok(children)
            })?;
        for (row_id, version_xmin, row) in children {
            if !visited.insert((child_schema.id, row_id)) {
                continue;
            }
            let action = if new_row.is_some() {
                foreign_key.on_update
            } else {
                foreign_key.on_delete
            };
            if matches!(action, ForeignKeyAction::NoAction)
                && is_foreign_key_deferred(&foreign_key, deferred_constraints, defer_all)
            {
                continue;
            }
            match action {
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    return Err(PgError::create(
                        SqlState::ForeignKeyViolation,
                        format!(
                            "update or delete on table {:?} violates foreign key constraint {:?} on table {:?}",
                            parent_schema.name, foreign_key.name, child_schema.name
                        ),
                    ));
                }
                ForeignKeyAction::Cascade if new_row.is_none() => {
                    apply_referencing_foreign_key_actions(
                        state,
                        &child_schema,
                        &row,
                        None,
                        xid,
                        &snapshot,
                        deferred_constraints,
                        defer_all,
                        visited,
                        context,
                    )?;
                    state
                        .tables
                        .get_mut(&child_schema.id)
                        .expect("catalog table must have storage")
                        .mark_version_deleted(row_id, version_xmin, xid, context.command_id);
                    state.mark_table_touched(xid, child_schema.id);
                }
                ForeignKeyAction::Cascade => {
                    let mut updated = row.clone();
                    for (child, value) in child_indexes
                        .iter()
                        .zip(new_key.as_ref().expect("update has a new key"))
                    {
                        updated[*child] = value.clone();
                    }
                    apply_cascaded_row_update(
                        state,
                        &child_schema,
                        row_id,
                        version_xmin,
                        &row,
                        updated,
                        xid,
                        &snapshot,
                        deferred_constraints,
                        defer_all,
                        visited,
                        context,
                    )?;
                }
                ForeignKeyAction::SetNull => {
                    let mut updated = row.clone();
                    for child in &child_indexes {
                        updated[*child] = Value::Null;
                    }
                    apply_cascaded_row_update(
                        state,
                        &child_schema,
                        row_id,
                        version_xmin,
                        &row,
                        updated,
                        xid,
                        &snapshot,
                        deferred_constraints,
                        defer_all,
                        visited,
                        context,
                    )?;
                }
                ForeignKeyAction::SetDefault => {
                    let mut updated = row.clone();
                    for child in &child_indexes {
                        updated[*child] =
                            evaluate_column_default(&child_schema.columns[*child], context)?;
                    }
                    apply_cascaded_row_update(
                        state,
                        &child_schema,
                        row_id,
                        version_xmin,
                        &row,
                        updated,
                        xid,
                        &snapshot,
                        deferred_constraints,
                        defer_all,
                        visited,
                        context,
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn apply_cascaded_row_update(
    state: &mut DatabaseState,
    schema: &TableSchema,
    row_id: RowId,
    version_xmin: Xid,
    old_row: &[Value],
    updated: Vec<Value>,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    visited: &mut BTreeSet<(TableId, RowId)>,
    context: &StatementExecutionContext,
) -> Result<()> {
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
            None,
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
    apply_referencing_foreign_key_actions(
        state,
        schema,
        old_row,
        Some(&updated),
        xid,
        snapshot,
        deferred_constraints,
        defer_all,
        visited,
        context,
    )?;
    Ok(())
}
