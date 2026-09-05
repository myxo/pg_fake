use super::*;

struct AlteredRow {
    row_id: RowId,
    version_xmin: Xid,
    original: Vec<Value>,
    row: Vec<Value>,
}

enum ReferencedDependency<'a> {
    Constraint {
        columns: &'a [String],
        primary_key: bool,
    },
    Column {
        name: &'a str,
        primary_columns: &'a [String],
    },
}

impl ReferencedDependency<'_> {
    fn does_match(&self, foreign_key: &ForeignKey, table_id: TableId) -> bool {
        if foreign_key.foreign_table_id != table_id {
            return false;
        }
        match self {
            Self::Constraint {
                columns,
                primary_key,
            } => {
                (foreign_key.referred_columns.is_empty() && *primary_key)
                    || foreign_key.referred_columns.as_slice() == *columns
            }
            Self::Column {
                name,
                primary_columns,
            } => {
                foreign_key
                    .referred_columns
                    .iter()
                    .any(|column| column == *name)
                    || (foreign_key.referred_columns.is_empty()
                        && primary_columns.iter().any(|column| column == *name))
            }
        }
    }
}

pub(super) fn execute_alter_table(
    state: &mut DatabaseState,
    alter: &ast::AlterTable,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    let existing_sequences = state
        .sequence_values
        .lock()
        .expect("sequence storage is poisoned")
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    let result = execute_alter_table_inner(
        state,
        alter,
        xid,
        snapshot,
        deferred_constraints,
        defer_all,
        context,
    );
    if result.is_err() {
        let mut values = state
            .sequence_values
            .lock()
            .expect("sequence storage is poisoned");
        let discarded = values
            .keys()
            .filter(|id| !existing_sequences.contains(id))
            .copied()
            .collect::<BTreeSet<_>>();
        values.retain(|id, _| existing_sequences.contains(id));
        drop(values);
        context.sequences.discard_sequences(&discarded);
    }
    result
}

fn execute_alter_table_inner(
    state: &mut DatabaseState,
    alter: &ast::AlterTable,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    if alter.only
        || alter.location.is_some()
        || alter.on_cluster.is_some()
        || alter.table_type.is_some()
    {
        return reject_unsupported("ALTER TABLE variant is not implemented");
    }
    let name = normalize_relation_name(&alter.name)?;
    let mut schema = match state.catalog.require_named_table(&name) {
        Ok(schema) => schema.clone(),
        Err(error) if alter.if_exists && error.sqlstate == SqlState::UndefinedTable => {
            return Ok(StatementResult::Affected(0));
        }
        Err(error) => return Err(error),
    };
    let visible_snapshot = snapshot.include_current_command();
    let versions = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .collect_visible_versions(&visible_snapshot, xid, &state.transactions);
    let mut rows = versions
        .into_iter()
        .map(|(row_id, version)| AlteredRow {
            row_id,
            version_xmin: version.xmin,
            original: version.row.clone(),
            row: version.row,
        })
        .collect::<Vec<_>>();

    for operation in &alter.operations {
        execute_alter_operation(state, &mut schema, &mut rows, operation, context)?;
    }

    state.catalog.replace_table(schema.clone())?;
    for altered in &rows {
        if altered.row == altered.original {
            continue;
        }
        state
            .tables
            .get_mut(&schema.id)
            .expect("catalog table must have storage")
            .append_updated_version(
                altered.row_id,
                altered.version_xmin,
                xid,
                context.command_id,
                altered.row.clone(),
                None,
            );
    }
    if rows.iter().any(|row| row.row != row.original) {
        state.mark_table_touched(xid, schema.id);
    }
    state
        .tables
        .get_mut(&schema.id)
        .expect("catalog table must have storage")
        .replace_schema(schema.clone());

    let mut validation_schema = schema.clone();
    validation_schema
        .constraints
        .retain(|constraint| match constraint {
            crate::catalog::Constraint::Check { validated, .. } => *validated,
            crate::catalog::Constraint::ForeignKey(foreign_key) => foreign_key.validated,
            crate::catalog::Constraint::PrimaryKey { .. }
            | crate::catalog::Constraint::Unique { .. } => true,
        });
    for altered in &rows {
        validate_not_null(&validation_schema, &altered.row)?;
        validate_check_constraints(&validation_schema, &altered.row, context)?;
        validate_row_foreign_keys(
            state,
            &validation_schema,
            &altered.row,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            &[],
        )?;
        if state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .has_visible_unique_conflict(
                &altered.row,
                snapshot,
                xid,
                &state.transactions,
                Some(altered.row_id),
                None,
                None,
                None,
                context,
            )
        {
            return Err(PgError::create(
                SqlState::UniqueViolation,
                format!("could not create unique constraint on {:?}", schema.name),
            ));
        }
    }
    validate_foreign_key_definitions(&state.catalog, &schema)?;
    Ok(StatementResult::Affected(0))
}

fn execute_alter_operation(
    state: &mut DatabaseState,
    schema: &mut TableSchema,
    rows: &mut [AlteredRow],
    operation: &ast::AlterTableOperation,
    context: &StatementExecutionContext,
) -> Result<()> {
    match operation {
        ast::AlterTableOperation::AddColumn {
            if_not_exists,
            column_def,
            column_position,
            ..
        } => {
            if column_position.is_some() {
                return reject_unsupported("ALTER TABLE column position is not implemented");
            }
            let name = normalize_identifier(&column_def.name);
            if schema.columns.iter().any(|column| column.name == name) {
                if *if_not_exists {
                    return Ok(());
                }
                return Err(PgError::create(
                    SqlState::DuplicateColumn,
                    format!("column {name:?} already exists"),
                ));
            }
            let mut column = create_altered_column(state, schema, column_def)?;
            validate_column_default(&column)?;
            let mut default_context = context.clone();
            default_context.sequences = context.sequences.replace_catalog(&state.catalog);
            let mut values = Vec::with_capacity(rows.len());
            for _ in rows.iter() {
                values.push(evaluate_column_default(&column, &default_context)?);
            }
            for (row, value) in rows.iter_mut().zip(values) {
                row.row.push(value);
            }
            for option in &column_def.options {
                match &option.option {
                    ast::ColumnOption::PrimaryKey(_) => {
                        column.nullable = false;
                        schema
                            .constraints
                            .push(crate::catalog::Constraint::PrimaryKey {
                                id: state.catalog.allocate_constraint_id(),
                                name: option
                                    .name
                                    .as_ref()
                                    .map(normalize_identifier)
                                    .unwrap_or_else(|| format!("{}_pkey", schema.name)),
                                columns: vec![name.clone()],
                            });
                    }
                    ast::ColumnOption::Unique(_) => {
                        schema.constraints.push(crate::catalog::Constraint::Unique {
                            id: state.catalog.allocate_constraint_id(),
                            name: option
                                .name
                                .as_ref()
                                .map(normalize_identifier)
                                .unwrap_or_else(|| format!("{}_{}_key", schema.name, name)),
                            columns: vec![name.clone()],
                        });
                    }
                    ast::ColumnOption::Check(check) => {
                        schema.constraints.push(crate::catalog::Constraint::Check {
                            id: state.catalog.allocate_constraint_id(),
                            name: option
                                .name
                                .as_ref()
                                .map(normalize_identifier)
                                .unwrap_or_else(|| {
                                    generate_constraint_name(
                                        format!("{}_{}_check", schema.name, name),
                                        &schema.constraints,
                                    )
                                }),
                            expression: check.expr.clone(),
                            validated: true,
                        });
                    }
                    ast::ColumnOption::ForeignKey(foreign_key) => {
                        let mut extended_schema = schema.clone();
                        extended_schema.columns.push(column.clone());
                        schema.constraints.push(create_foreign_key_constraint(
                            state,
                            &extended_schema,
                            option.name.as_ref(),
                            vec![name.clone()],
                            foreign_key,
                            true,
                        )?);
                    }
                    ast::ColumnOption::Null
                    | ast::ColumnOption::NotNull
                    | ast::ColumnOption::Default(_)
                    | ast::ColumnOption::Generated { .. } => {}
                    option => {
                        return reject_unsupported(format!(
                            "ALTER TABLE column option is not implemented: {option}"
                        ));
                    }
                }
            }
            schema.columns.push(column);
        }
        ast::AlterTableOperation::DropColumn {
            column_names,
            if_exists,
            drop_behavior,
            ..
        } => {
            for column_name in column_names {
                let name = normalize_identifier(column_name);
                let Some(index) = schema.columns.iter().position(|column| column.name == name)
                else {
                    if *if_exists {
                        continue;
                    }
                    return Err(PgError::create(
                        SqlState::UndefinedColumn,
                        format!("column {name:?} does not exist"),
                    ));
                };
                remove_column_dependencies(state, schema, &name, *drop_behavior)?;
                schema.columns.remove(index);
                for row in rows.iter_mut() {
                    row.row.remove(index);
                }
            }
        }
        ast::AlterTableOperation::RenameColumn {
            old_column_name,
            new_column_name,
        } => {
            let old_name = normalize_identifier(old_column_name);
            let new_name = normalize_identifier(new_column_name);
            if schema.columns.iter().any(|column| column.name == new_name) {
                return Err(PgError::create(
                    SqlState::DuplicateColumn,
                    format!("column {new_name:?} already exists"),
                ));
            }
            let column = schema
                .columns
                .iter_mut()
                .find(|column| column.name == old_name)
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::UndefinedColumn,
                        format!("column {old_name:?} does not exist"),
                    )
                })?;
            column.name = new_name.clone();
            for trigger in &mut schema.triggers {
                for event in &mut trigger.definition.events {
                    if let ast::TriggerEvent::Update(columns) = event {
                        for column in columns
                            .iter_mut()
                            .filter(|column| normalize_identifier(column) == old_name)
                        {
                            column.value = new_name.clone();
                        }
                    }
                }
            }
            rename_schema_expressions(schema, &old_name, &new_name);
            super::views::rename_column_references(
                &mut state.catalog,
                schema,
                &old_name,
                &new_name,
            );
            state
                .catalog
                .rename_column_dependencies(schema.id, &old_name, &new_name);
            rename_local_constraint_columns(schema, &old_name, &new_name);
        }
        ast::AlterTableOperation::RenameTable { table_name } => {
            let name = match table_name {
                ast::RenameTableNameKind::To(name) | ast::RenameTableNameKind::As(name) => name,
            };
            let new_name = normalize_relation_name(name)?;
            if new_name.schema.is_some() {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "ALTER TABLE RENAME TO does not accept a qualified name",
                ));
            }
            super::views::rename_table_references(&mut state.catalog, schema.id, &new_name.name);
            for trigger in &mut schema.triggers {
                let ast::ObjectNamePart::Identifier(name) = trigger
                    .definition
                    .table_name
                    .0
                    .last_mut()
                    .expect("trigger table name is non-empty")
                else {
                    unreachable!("trigger table name ends in an identifier")
                };
                name.value = new_name.name.clone();
            }
            schema.name = new_name.name;
            state
                .catalog
                .rename_table_dependencies(schema.id, &schema.name);
            for constraint in &mut schema.constraints {
                if let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint
                    && foreign_key.foreign_table_id == schema.id
                {
                    foreign_key.foreign_table.name = schema.name.clone();
                }
            }
        }
        ast::AlterTableOperation::AlterColumn { column_name, op } => {
            alter_column(state, schema, rows, column_name, op, context)?;
        }
        ast::AlterTableOperation::AddConstraint {
            constraint,
            not_valid,
        } => {
            let constraint = create_table_constraint(state, schema, constraint, *not_valid)?;
            let name = constraint
                .get_name()
                .expect("supported constraints must be named");
            if schema
                .constraints
                .iter()
                .any(|existing| existing.get_name() == Some(name))
            {
                return Err(PgError::create(
                    SqlState::DuplicateObject,
                    format!("constraint {name:?} already exists"),
                ));
            }
            if let crate::catalog::Constraint::PrimaryKey { columns, .. } = &constraint {
                for name in columns {
                    schema
                        .columns
                        .iter_mut()
                        .find(|column| column.name == *name)
                        .expect("constraint column was validated")
                        .nullable = false;
                }
            }
            schema.constraints.push(constraint);
        }
        ast::AlterTableOperation::DropConstraint {
            if_exists,
            name,
            drop_behavior,
        } => {
            let name = normalize_identifier(name);
            let Some(index) = schema
                .constraints
                .iter()
                .position(|constraint| constraint.get_name() == Some(&name))
            else {
                if *if_exists {
                    return Ok(());
                }
                return Err(PgError::create(
                    SqlState::UndefinedObject,
                    format!("constraint {name:?} does not exist"),
                ));
            };
            if state
                .catalog
                .has_dependent_views_for_constraint(schema.constraints[index].get_id())
            {
                return Err(PgError::create(
                    SqlState::DependentObjectsStillExist,
                    "cannot drop constraint because a view depends on it",
                ));
            }
            let dependency = match &schema.constraints[index] {
                crate::catalog::Constraint::PrimaryKey { columns, .. } => {
                    Some((columns.clone(), true))
                }
                crate::catalog::Constraint::Unique { columns, .. } => {
                    Some((columns.clone(), false))
                }
                crate::catalog::Constraint::Check { .. }
                | crate::catalog::Constraint::ForeignKey(_) => None,
            };
            if let Some((columns, primary_key)) = dependency {
                let dependency = ReferencedDependency::Constraint {
                    columns: &columns,
                    primary_key,
                };
                remove_local_referencing_foreign_keys(schema, index, &dependency, *drop_behavior)?;
                remove_referencing_foreign_keys(state, schema.id, dependency, *drop_behavior)?;
            }
            let index = schema
                .constraints
                .iter()
                .position(|constraint| constraint.get_name() == Some(&name))
                .expect("target constraint must remain after dependent removal");
            schema.constraints.remove(index);
        }
        ast::AlterTableOperation::ValidateConstraint { name } => {
            let name = normalize_identifier(name);
            let constraint = schema
                .constraints
                .iter_mut()
                .find(|constraint| constraint.get_name() == Some(&name))
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::UndefinedObject,
                        format!("constraint {name:?} does not exist"),
                    )
                })?;
            match constraint {
                crate::catalog::Constraint::Check { validated, .. } => *validated = true,
                crate::catalog::Constraint::ForeignKey(foreign_key) => foreign_key.validated = true,
                crate::catalog::Constraint::PrimaryKey { .. }
                | crate::catalog::Constraint::Unique { .. } => {}
            }
        }
        operation => {
            return reject_unsupported(format!(
                "ALTER TABLE operation is not implemented: {operation}"
            ));
        }
    }
    let mut constraint_names = BTreeSet::new();
    for constraint in &schema.constraints {
        let name = constraint
            .get_name()
            .expect("supported constraints must be named");
        if !constraint_names.insert(name) {
            return Err(PgError::create(
                SqlState::DuplicateObject,
                format!("constraint {name:?} already exists"),
            ));
        }
    }
    if schema
        .constraints
        .iter()
        .filter(|constraint| matches!(constraint, crate::catalog::Constraint::PrimaryKey { .. }))
        .count()
        > 1
    {
        return Err(PgError::create(
            SqlState::InvalidTableDefinition,
            "multiple primary keys for table are not allowed",
        ));
    }
    validate_check_constraint_types(schema)?;
    Ok(())
}

fn create_altered_column(
    state: &mut DatabaseState,
    schema: &TableSchema,
    definition: &ast::ColumnDef,
) -> Result<ColumnDef> {
    let serial_type = match definition
        .data_type
        .to_string()
        .to_ascii_lowercase()
        .as_str()
    {
        "smallserial" | "serial2" => Some(BaseType::Int2),
        "serial" | "serial4" => Some(BaseType::Int4),
        "bigserial" | "serial8" => Some(BaseType::Int8),
        _ => None,
    };
    let data_type = match serial_type {
        Some(base) => PgType::create(base),
        None => coercion::convert_ast_data_type(&definition.data_type)?,
    };
    let mut nullable = true;
    let mut default = None;
    let mut default_sequence = None;
    let mut identity = None;
    let mut sequence_options = None;
    for option in &definition.options {
        match &option.option {
            ast::ColumnOption::Null => nullable = true,
            ast::ColumnOption::NotNull => nullable = false,
            ast::ColumnOption::Default(expression) => {
                if serial_type.is_some() || identity.is_some() {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "multiple default values specified for column",
                    ));
                }
                default = Some(expression.clone());
                default_sequence =
                    resolve_default_sequence(&state.catalog, expression, schema.persistence)?;
            }
            ast::ColumnOption::PrimaryKey(_)
            | ast::ColumnOption::Unique(_)
            | ast::ColumnOption::Check(_)
            | ast::ColumnOption::ForeignKey(_) => {}
            ast::ColumnOption::Generated {
                generated_as,
                sequence_options: options,
                generation_expr,
                generation_expr_mode,
                generated_keyword,
            } => {
                if serial_type.is_some()
                    || default.is_some()
                    || identity.is_some()
                    || generation_expr.is_some()
                    || generation_expr_mode.is_some()
                    || !generated_keyword
                {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "invalid identity column declaration",
                    ));
                }
                identity = Some(match generated_as {
                    ast::GeneratedAs::Always => IdentityKind::Always,
                    ast::GeneratedAs::ByDefault => IdentityKind::ByDefault,
                    ast::GeneratedAs::ExpStored => {
                        return Err(PgError::create(
                            SqlState::SyntaxError,
                            "invalid identity column declaration",
                        ));
                    }
                });
                sequence_options = options.clone();
            }
            option => {
                return reject_unsupported(format!(
                    "ALTER TABLE column option is not implemented: {option}"
                ));
            }
        }
    }
    if identity.is_some()
        && !matches!(
            data_type.base,
            BaseType::Int2 | BaseType::Int4 | BaseType::Int8
        )
    {
        return Err(PgError::create(
            SqlState::DatatypeMismatch,
            "identity column type must be smallint, integer, or bigint",
        ));
    }
    if serial_type.is_some() || identity.is_some() {
        let column_name = normalize_identifier(&definition.name);
        let resolved_table = ResolvedRelationName {
            schema_id: schema.schema_id,
            name: schema.name.clone(),
        };
        let sequence_name =
            create_generated_sequence_name(&state.catalog, &[], &resolved_table, &column_name);
        let mut sequence = sequences::create_sequence_schema_for_type(
            sequence_name.clone(),
            data_type.base,
            sequence_options.as_deref().unwrap_or(&[]),
        )?;
        sequence.owned_by = Some((schema.id, column_name));
        let initial = SequenceValueState {
            last_value: sequence.start_value,
            is_called: false,
        };
        let id = state.catalog.create_named_sequence(
            ResolvedRelationName {
                schema_id: schema.schema_id,
                name: sequence_name.clone(),
            },
            sequence,
        )?;
        state
            .sequence_values
            .lock()
            .expect("sequence storage is poisoned")
            .insert(id, initial);
        nullable = false;
        default_sequence = Some(ResolvedRelationName {
            schema_id: schema.schema_id,
            name: sequence_name,
        });
    }
    Ok(ColumnDef {
        name: normalize_identifier(&definition.name),
        data_type,
        nullable,
        default,
        default_sequence,
        identity,
    })
}

fn alter_column(
    state: &DatabaseState,
    schema: &mut TableSchema,
    rows: &mut [AlteredRow],
    column_name: &ast::Ident,
    operation: &ast::AlterColumnOperation,
    context: &StatementExecutionContext,
) -> Result<()> {
    let name = normalize_identifier(column_name);
    let index = schema
        .columns
        .iter()
        .position(|column| column.name == name)
        .ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedColumn,
                format!("column {name:?} does not exist"),
            )
        })?;
    match operation {
        ast::AlterColumnOperation::SetNotNull => schema.columns[index].nullable = false,
        ast::AlterColumnOperation::DropNotNull => {
            if schema.constraints.iter().any(|constraint| {
                matches!(constraint, crate::catalog::Constraint::PrimaryKey { columns, .. } if columns.contains(&name))
            }) {
                return Err(PgError::create(
                    SqlState::InvalidTableDefinition,
                    format!("column {name:?} is in a primary key"),
                ));
            }
            schema.columns[index].nullable = true;
        }
        ast::AlterColumnOperation::SetDefault { value } => {
            schema.columns[index].default_sequence =
                resolve_default_sequence(&state.catalog, value, schema.persistence)?;
            schema.columns[index].default = Some(value.clone());
            validate_column_default(&schema.columns[index])?;
        }
        ast::AlterColumnOperation::DropDefault => {
            schema.columns[index].default = None;
            schema.columns[index].default_sequence = None;
        }
        ast::AlterColumnOperation::SetDataType {
            data_type, using, ..
        } => {
            if super::views::has_view_column_dependency(&state.catalog, schema.id, &name) {
                return Err(PgError::create(
                    SqlState::FeatureNotSupported,
                    "cannot alter column type because a view depends on it",
                ));
            }
            let target = coercion::convert_ast_data_type(data_type)?;
            let old_schema = schema.clone();
            let source = old_schema.columns[index].data_type.base;
            let values = rows
                .iter()
                .map(|row| match using {
                    Some(expression) => evaluate_assignment_expression(
                        expression,
                        target,
                        &old_schema,
                        &row.row,
                        context,
                    ),
                    None => coercion::coerce(
                        row.row[index].clone(),
                        source,
                        target,
                        CastContext::Assignment,
                    ),
                })
                .collect::<Result<Vec<_>>>()?;
            for (row, value) in rows.iter_mut().zip(values) {
                row.row[index] = value;
            }
            schema.columns[index].data_type = target;
            validate_column_default(&schema.columns[index])?;
        }
        ast::AlterColumnOperation::AddGenerated { .. } => {
            return reject_unsupported("ALTER TABLE ADD GENERATED is not implemented");
        }
    }
    Ok(())
}

fn create_table_constraint(
    state: &mut DatabaseState,
    schema: &TableSchema,
    constraint: &ast::TableConstraint,
    not_valid: bool,
) -> Result<crate::catalog::Constraint> {
    match constraint {
        ast::TableConstraint::PrimaryKey(primary_key) => {
            if not_valid {
                return reject_unsupported("PRIMARY KEY constraints cannot be marked NOT VALID");
            }
            let columns = primary_key
                .columns
                .iter()
                .map(resolve_index_column_name)
                .collect::<Result<Vec<_>>>()?;
            validate_constraint_columns(schema, &columns)?;
            Ok(crate::catalog::Constraint::PrimaryKey {
                id: state.catalog.allocate_constraint_id(),
                name: primary_key
                    .name
                    .as_ref()
                    .map(normalize_identifier)
                    .unwrap_or_else(|| format!("{}_pkey", schema.name)),
                columns,
            })
        }
        ast::TableConstraint::Unique(unique) => {
            if not_valid {
                return reject_unsupported("UNIQUE constraints cannot be marked NOT VALID");
            }
            let columns = unique
                .columns
                .iter()
                .map(resolve_index_column_name)
                .collect::<Result<Vec<_>>>()?;
            validate_constraint_columns(schema, &columns)?;
            Ok(crate::catalog::Constraint::Unique {
                id: state.catalog.allocate_constraint_id(),
                name: unique
                    .name
                    .as_ref()
                    .map(normalize_identifier)
                    .unwrap_or_else(|| format!("{}_{}_key", schema.name, columns.join("_"))),
                columns,
            })
        }
        ast::TableConstraint::Check(check) => {
            let base = find_first_referenced_column(&check.expr, &schema.columns).map_or_else(
                || format!("{}_check", schema.name),
                |column| format!("{}_{column}_check", schema.name),
            );
            Ok(crate::catalog::Constraint::Check {
                id: state.catalog.allocate_constraint_id(),
                name: check
                    .name
                    .as_ref()
                    .map(normalize_identifier)
                    .unwrap_or_else(|| generate_constraint_name(base, &schema.constraints)),
                expression: check.expr.clone(),
                validated: !not_valid,
            })
        }
        ast::TableConstraint::ForeignKey(foreign_key) => create_foreign_key_constraint(
            state,
            schema,
            foreign_key.name.as_ref(),
            foreign_key
                .columns
                .iter()
                .map(normalize_identifier)
                .collect(),
            foreign_key,
            !not_valid,
        ),
        constraint => reject_unsupported(format!(
            "ALTER TABLE constraint is not implemented: {constraint}"
        )),
    }
}

fn create_foreign_key_constraint(
    state: &mut DatabaseState,
    schema: &TableSchema,
    name: Option<&ast::Ident>,
    columns: Vec<String>,
    foreign_key: &ast::ForeignKeyConstraint,
    validated: bool,
) -> Result<crate::catalog::Constraint> {
    validate_constraint_columns(schema, &columns)?;
    let foreign_table = normalize_relation_name(&foreign_key.foreign_table)?;
    let foreign_table_id = if foreign_table.name == schema.name
        && foreign_table.schema.as_deref().is_none_or(|name| {
            state
                .catalog
                .require_schema(name)
                .is_ok_and(|candidate| candidate.id == schema.schema_id)
        }) {
        schema.id
    } else {
        state.catalog.require_named_table(&foreign_table)?.id
    };
    let default_name = format!("{}_{}_fkey", schema.name, columns.join("_"));
    Ok(crate::catalog::Constraint::ForeignKey(ForeignKey {
        id: state.catalog.allocate_constraint_id(),
        name: resolve_foreign_key_name(name, default_name),
        columns,
        foreign_table,
        foreign_table_id,
        referred_columns: foreign_key
            .referred_columns
            .iter()
            .map(normalize_identifier)
            .collect(),
        on_delete: convert_referential_action(foreign_key.on_delete),
        on_update: convert_referential_action(foreign_key.on_update),
        deferrable: foreign_key
            .characteristics
            .is_some_and(|characteristics| characteristics.deferrable.unwrap_or(false)),
        initially_deferred: foreign_key.characteristics.is_some_and(|characteristics| {
            characteristics.initially == Some(ast::DeferrableInitial::Deferred)
        }),
        match_kind: foreign_key.match_kind,
        validated,
    }))
}

fn validate_constraint_columns(schema: &TableSchema, columns: &[String]) -> Result<()> {
    for name in columns {
        if !schema.columns.iter().any(|column| &column.name == name) {
            return Err(PgError::create(
                SqlState::UndefinedColumn,
                format!("column {name:?} does not exist"),
            ));
        }
    }
    Ok(())
}

fn remove_column_dependencies(
    state: &mut DatabaseState,
    schema: &mut TableSchema,
    column: &str,
    behavior: Option<ast::DropBehavior>,
) -> Result<()> {
    if super::views::has_view_column_dependency(&state.catalog, schema.id, column) {
        return Err(PgError::create(
            SqlState::DependentObjectsStillExist,
            "cannot drop column because a view depends on it",
        ));
    }
    super::views::preserve_column_drop_references(&mut state.catalog, schema, column);
    let primary_columns = schema
        .constraints
        .iter()
        .find_map(|constraint| match constraint {
            crate::catalog::Constraint::PrimaryKey { columns, .. } => Some(columns.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let dependency = ReferencedDependency::Column {
        name: column,
        primary_columns: &primary_columns,
    };
    remove_local_referencing_foreign_keys(schema, usize::MAX, &dependency, behavior)?;
    remove_referencing_foreign_keys(state, schema.id, dependency, behavior)?;
    state.catalog.drop_column_owned_sequences(schema.id, column);
    schema.indexes.retain(|index| {
        !index.columns.iter().any(|key| key.name == column)
            && !index.include.iter().any(|included| included == column)
            && !index
                .predicate
                .as_ref()
                .is_some_and(|predicate| expression_references_column(predicate, column))
    });
    schema.constraints.retain(|constraint| match constraint {
        crate::catalog::Constraint::PrimaryKey { columns, .. }
        | crate::catalog::Constraint::Unique { columns, .. } => {
            !columns.contains(&column.to_owned())
        }
        crate::catalog::Constraint::ForeignKey(foreign_key) => {
            !foreign_key.columns.contains(&column.to_owned())
        }
        crate::catalog::Constraint::Check { expression, .. } => {
            !expression_references_column(expression, column)
        }
    });
    Ok(())
}

fn remove_referencing_foreign_keys(
    state: &mut DatabaseState,
    table_id: TableId,
    dependency: ReferencedDependency<'_>,
    behavior: Option<ast::DropBehavior>,
) -> Result<()> {
    let mut tables = state
        .catalog
        .iterate_tables()
        .filter(|table| table.id != table_id)
        .cloned()
        .collect::<Vec<_>>();
    let depends = |foreign_key: &ForeignKey| dependency.does_match(foreign_key, table_id);
    let referenced = tables.iter().any(|table| {
        table.constraints.iter().any(|constraint| {
            matches!(constraint, crate::catalog::Constraint::ForeignKey(foreign_key) if depends(foreign_key))
        })
    });
    if referenced && behavior != Some(ast::DropBehavior::Cascade) {
        return Err(PgError::create(
            SqlState::DependentObjectsStillExist,
            "cannot drop constraint because other objects depend on it",
        ));
    }
    for table in &mut tables {
        let before = table.constraints.len();
        table.constraints.retain(|constraint| {
            !matches!(constraint, crate::catalog::Constraint::ForeignKey(foreign_key) if depends(foreign_key))
        });
        if table.constraints.len() != before {
            state.catalog.replace_table(table.clone())?;
        }
    }
    Ok(())
}

fn remove_local_referencing_foreign_keys(
    schema: &mut TableSchema,
    dropped_constraint: usize,
    dependency: &ReferencedDependency<'_>,
    behavior: Option<ast::DropBehavior>,
) -> Result<()> {
    let depends = |index: usize, constraint: &crate::catalog::Constraint| {
        if index == dropped_constraint {
            return false;
        }
        let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
            return false;
        };
        if let ReferencedDependency::Column { name, .. } = dependency
            && foreign_key.columns.iter().any(|column| column == *name)
        {
            return false;
        }
        dependency.does_match(foreign_key, schema.id)
    };
    if schema
        .constraints
        .iter()
        .enumerate()
        .any(|(index, constraint)| depends(index, constraint))
        && behavior != Some(ast::DropBehavior::Cascade)
    {
        return Err(PgError::create(
            SqlState::DependentObjectsStillExist,
            "cannot drop object because other objects depend on it",
        ));
    }
    let mut index = 0;
    schema.constraints.retain(|constraint| {
        let keep = !depends(index, constraint);
        index += 1;
        keep
    });
    Ok(())
}

fn rename_local_constraint_columns(schema: &mut TableSchema, old_name: &str, new_name: &str) {
    for constraint in &mut schema.constraints {
        match constraint {
            crate::catalog::Constraint::PrimaryKey { columns, .. }
            | crate::catalog::Constraint::Unique { columns, .. } => {
                for column in columns {
                    if column == old_name {
                        *column = new_name.to_owned();
                    }
                }
            }
            crate::catalog::Constraint::ForeignKey(foreign_key) => {
                for column in &mut foreign_key.columns {
                    if column == old_name {
                        *column = new_name.to_owned();
                    }
                }
                if foreign_key.foreign_table_id == schema.id {
                    for column in &mut foreign_key.referred_columns {
                        if column == old_name {
                            *column = new_name.to_owned();
                        }
                    }
                }
            }
            crate::catalog::Constraint::Check { .. } => {}
        }
    }
}

fn rename_schema_expressions(schema: &mut TableSchema, old_name: &str, new_name: &str) {
    for column in &mut schema.columns {
        if let Some(default) = &mut column.default {
            rename_expression_column(default, old_name, new_name);
        }
    }
    for constraint in &mut schema.constraints {
        if let crate::catalog::Constraint::Check { expression, .. } = constraint {
            rename_expression_column(expression, old_name, new_name);
        }
    }
    for index in &mut schema.indexes {
        for column in &mut index.columns {
            if column.name == old_name {
                column.name = new_name.to_owned();
            }
        }
        for column in &mut index.include {
            if column == old_name {
                *column = new_name.to_owned();
            }
        }
        if let Some(predicate) = &mut index.predicate {
            rename_expression_column(predicate, old_name, new_name);
        }
    }
}

fn rename_expression_column(expression: &mut ast::Expr, old_name: &str, new_name: &str) {
    let _ = ast::visit_expressions_mut(expression, |nested| {
        match nested {
            ast::Expr::Identifier(identifier) if normalize_identifier(identifier) == old_name => {
                identifier.value = new_name.to_owned();
            }
            ast::Expr::CompoundIdentifier(identifiers)
                if identifiers
                    .last()
                    .is_some_and(|identifier| normalize_identifier(identifier) == old_name) =>
            {
                identifiers.last_mut().expect("identifier exists").value = new_name.to_owned();
            }
            _ => {}
        }
        std::ops::ControlFlow::<()>::Continue(())
    });
}

fn expression_references_column(expression: &ast::Expr, column: &str) -> bool {
    let mut referenced = false;
    let _ = ast::visit_expressions(expression, |nested| {
        let matches = match nested {
            ast::Expr::Identifier(identifier) => normalize_identifier(identifier) == column,
            ast::Expr::CompoundIdentifier(identifiers) => identifiers
                .last()
                .is_some_and(|identifier| normalize_identifier(identifier) == column),
            _ => false,
        };
        if matches {
            referenced = true;
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    });
    referenced
}
