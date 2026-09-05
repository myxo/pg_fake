use super::*;

pub(super) fn execute_create_index(
    state: &mut DatabaseState,
    create: &ast::CreateIndex,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    if create.concurrently
        || create.nulls_distinct.is_some()
        || !create.with.is_empty()
        || !create.index_options.is_empty()
        || !create.alter_options.is_empty()
        || create
            .using
            .as_ref()
            .is_some_and(|using| !matches!(using, ast::IndexType::BTree))
    {
        return reject_unsupported("CREATE INDEX option is not implemented");
    }
    let name = create
        .name
        .as_ref()
        .ok_or_else(|| PgError::create(SqlState::SyntaxError, "index name is required"))?;
    let table_name = normalize_relation_name(&create.table_name)?;
    let table = state.catalog.require_named_table(&table_name)?.clone();
    let requested_name = normalize_relation_name(name)?;
    let resolved_name = match requested_name.schema.as_deref() {
        None => ResolvedRelationName {
            schema_id: table.schema_id,
            name: requested_name.name,
        },
        Some(schema) => {
            let schema_id = state.catalog.require_schema(schema)?.id;
            if schema_id != table.schema_id {
                return Err(PgError::create(
                    SqlState::InvalidObjectDefinition,
                    "index and table must be in the same schema",
                ));
            }
            ResolvedRelationName {
                schema_id,
                name: requested_name.name,
            }
        }
    };
    if state.catalog.has_resolved_relation(&resolved_name) {
        if create.if_not_exists {
            return Ok(StatementResult::Affected(0));
        }
        return Err(PgError::create(
            SqlState::DuplicateTable,
            format!("relation {:?} already exists", resolved_name.name),
        ));
    }
    if create.columns.is_empty() || create.columns.len() > 4 {
        return reject_unsupported("indexes require one through four key columns");
    }

    let mut key_names = BTreeSet::new();
    let mut columns = Vec::with_capacity(create.columns.len());
    for column in &create.columns {
        if column.operator_class.is_some()
            || column.column.options.nulls_first.is_some()
            || column.column.with_fill.is_some()
        {
            return reject_unsupported("index column option is not implemented");
        }
        let ast::Expr::Identifier(identifier) = &column.column.expr else {
            return reject_unsupported("index expressions are not implemented");
        };
        let name = normalize_identifier(identifier);
        let definition = table
            .columns
            .iter()
            .find(|column| column.name == name)
            .ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedColumn,
                    format!("column {name:?} does not exist"),
                )
            })?;
        validate_btree_key_type(definition.data_type.base)?;
        if !key_names.insert(name.clone()) {
            return Err(PgError::create(
                SqlState::DuplicateColumn,
                format!("column {name:?} appears twice in index"),
            ));
        }
        columns.push(IndexColumnDefinition {
            name,
            descending: !resolve_order_ascending(&column.column.options)?,
        });
    }

    let mut include_names = BTreeSet::new();
    let include = create
        .include
        .iter()
        .map(|identifier| {
            let name = normalize_identifier(identifier);
            if !table.columns.iter().any(|column| column.name == name) {
                return Err(PgError::create(
                    SqlState::UndefinedColumn,
                    format!("column {name:?} does not exist"),
                ));
            }
            if key_names.contains(&name) || !include_names.insert(name.clone()) {
                return Err(PgError::create(
                    SqlState::DuplicateColumn,
                    format!("column {name:?} appears twice in index"),
                ));
            }
            Ok(name)
        })
        .collect::<Result<Vec<_>>>()?;

    if let Some(predicate) = &create.predicate {
        validate_index_predicate(predicate, &table)?;
    }
    if create.unique {
        validate_unique_index_rows(
            state,
            &table,
            &columns,
            create.predicate.as_ref(),
            xid,
            snapshot,
            context,
        )?;
    }

    let mut table = table;
    table.indexes.push(IndexSchema {
        id: state.catalog.allocate_index_id(),
        name: resolved_name.name,
        unique: create.unique,
        columns,
        include,
        predicate: create.predicate.clone(),
    });
    state.catalog.replace_table(table.clone())?;
    state
        .tables
        .get_mut(&table.id)
        .expect("catalog table must have storage")
        .replace_schema(table);
    Ok(StatementResult::Affected(0))
}

pub(super) fn execute_drop_indexes(
    state: &mut DatabaseState,
    names: &[ast::ObjectName],
    if_exists: bool,
    cascade: bool,
    restrict: bool,
) -> Result<StatementResult> {
    if cascade || restrict {
        return reject_unsupported("DROP INDEX behavior is not implemented");
    }
    let mut tables = BTreeMap::new();
    let mut dropped = BTreeSet::new();
    for name in names {
        let name = normalize_relation_name(name)?;
        let (table, index) = match state.catalog.require_named_index(&name) {
            Ok(found) => found,
            Err(error) if if_exists && error.sqlstate == SqlState::UndefinedObject => continue,
            Err(error) => return Err(error),
        };
        if dropped.insert(index.id) {
            tables.entry(table.id).or_insert_with(|| table.clone());
        }
    }
    for table in tables.values_mut() {
        table.indexes.retain(|index| !dropped.contains(&index.id));
    }
    replace_index_tables(state, tables)?;
    Ok(StatementResult::Affected(0))
}

pub(super) fn execute_alter_index(
    state: &mut DatabaseState,
    if_exists: bool,
    name: &ast::ObjectName,
    operation: &ast::AlterIndexOperation,
) -> Result<StatementResult> {
    let name = normalize_relation_name(name)?;
    let (table, index) = match state.catalog.require_named_index(&name) {
        Ok(found) => found,
        Err(error) if if_exists && error.sqlstate == SqlState::UndefinedObject => {
            return Ok(StatementResult::Affected(0));
        }
        Err(error) => return Err(error),
    };
    let ast::AlterIndexOperation::RenameIndex { index_name } = operation;
    let new_name = normalize_relation_name(index_name)?;
    if new_name.schema.is_some() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "ALTER INDEX RENAME TO does not accept a qualified name",
        ));
    }
    if new_name.name == index.name {
        return Ok(StatementResult::Affected(0));
    }
    let target = ResolvedRelationName {
        schema_id: table.schema_id,
        name: new_name.name.clone(),
    };
    if state.catalog.has_resolved_relation(&target) {
        return Err(PgError::create(
            SqlState::DuplicateTable,
            format!("relation {:?} already exists", new_name.name),
        ));
    }
    let index_id = index.id;
    let mut table = table.clone();
    table
        .indexes
        .iter_mut()
        .find(|index| index.id == index_id)
        .expect("required index must belong to its table")
        .name = new_name.name;
    state.catalog.replace_table(table.clone())?;
    state
        .tables
        .get_mut(&table.id)
        .expect("catalog table must have storage")
        .replace_schema(table);
    Ok(StatementResult::Affected(0))
}

fn validate_unique_index_rows(
    state: &DatabaseState,
    schema: &TableSchema,
    columns: &[IndexColumnDefinition],
    predicate: Option<&ast::Expr>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<()> {
    let column_indexes = columns
        .iter()
        .map(|column| {
            schema
                .columns
                .iter()
                .position(|definition| definition.name == column.name)
                .expect("validated index column must exist")
        })
        .collect::<Vec<_>>();
    let rows = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .collect_visible_versions(
            &snapshot.include_current_command(),
            xid,
            &state.transactions,
        )
        .into_iter()
        .map(|(_, version)| version.row)
        .filter(|row| {
            predicate.is_none_or(|predicate| {
                evaluate_index_predicate(predicate, schema, row, context)
                    .expect("validated index predicate must evaluate")
            })
        })
        .filter(|row| column_indexes.iter().all(|column| !row[*column].is_null()))
        .collect::<Vec<_>>();
    for left in 0..rows.len() {
        for right in left + 1..rows.len() {
            let duplicate = column_indexes.iter().all(|column| {
                compare_values(&rows[left][*column], &rows[right][*column])
                    .is_ok_and(|ordering| ordering == Ordering::Equal)
            });
            if duplicate {
                return Err(PgError::create(
                    SqlState::UniqueViolation,
                    format!("could not create unique index on {:?}", schema.name),
                ));
            }
        }
    }
    Ok(())
}

fn replace_index_tables(
    state: &mut DatabaseState,
    tables: BTreeMap<TableId, TableSchema>,
) -> Result<()> {
    for table in tables.into_values() {
        state.catalog.replace_table(table.clone())?;
        state
            .tables
            .get_mut(&table.id)
            .expect("catalog table must have storage")
            .replace_schema(table);
    }
    Ok(())
}
