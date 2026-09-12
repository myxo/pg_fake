use super::{
    DatabaseState, SequenceValueState,
    column_defaults::validate_column_default,
    foreign_keys::{
        convert_referential_action, resolve_foreign_key_name, validate_foreign_key_definitions,
    },
    normalize_identifier, normalize_relation_name, normalize_unqualified_object_name,
    resolve_index_column_name,
    row_constraints::validate_check_constraint_types,
    sequences, validate_btree_key_type,
};
use crate::{
    StatementResult,
    catalog::{
        Catalog, ColumnDef, ConstraintId, ForeignKey, IdentityKind, ResolvedRelationName,
        SequenceSchema, TEMP_SCHEMA, TableId, TablePersistence, TableSchema,
    },
    coercion,
    error::{PgError, Result, SqlState, reject_unsupported},
    storage::Table,
    value::{BaseType, PgType},
};
use sqlparser::ast;
use std::collections::BTreeSet;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_create_table(
    state: &mut DatabaseState,
    create: &ast::CreateTable,
) -> Result<StatementResult> {
    if create.query.is_some() {
        return reject_unsupported("CREATE TABLE AS is not implemented");
    }
    if create.like.is_some() || create.clone.is_some() {
        return reject_unsupported("CREATE TABLE LIKE is not implemented");
    }
    if create.inherits.is_some() {
        return reject_unsupported("table inheritance is not implemented");
    }
    if create.partition_by.is_some() || create.partition_of.is_some() || create.for_values.is_some()
    {
        return reject_unsupported("table partitioning is not implemented");
    }
    if !matches!(create.table_options, ast::CreateTableOptions::None) {
        return reject_unsupported("CREATE TABLE options are not implemented");
    }
    let relation_name = normalize_relation_name(&create.name)?;
    let temporary = create.temporary || relation_name.schema.as_deref() == Some(TEMP_SCHEMA);
    if create.on_commit.is_some() && !temporary {
        return Err(PgError::create(
            SqlState::InvalidTableDefinition,
            "ON COMMIT can only be used on temporary tables",
        ));
    }
    let on_commit_drop = match create.on_commit {
        None | Some(ast::OnCommit::PreserveRows) => false,
        Some(ast::OnCommit::Drop) => true,
        Some(ast::OnCommit::DeleteRows) => {
            return reject_unsupported("ON COMMIT DELETE ROWS is not implemented");
        }
    };
    let resolved_name = state
        .catalog
        .resolve_creation_name(&relation_name, temporary)?;
    let table_name = resolved_name.name.clone();
    let persistence = if temporary {
        TablePersistence::Temporary { on_commit_drop }
    } else {
        TablePersistence::Permanent
    };
    if create.if_not_exists && state.catalog.has_resolved_relation(&resolved_name) {
        return Ok(StatementResult::Affected(0));
    }
    let mut columns = Vec::new();
    let mut constraints = Vec::new();
    let mut sequence_schemas = Vec::new();
    for column in &create.columns {
        let column_name = normalize_identifier(&column.name);
        let serial_type = match column.data_type.to_string().to_ascii_lowercase().as_str() {
            "smallserial" | "serial2" => Some(BaseType::Int2),
            "serial" | "serial4" => Some(BaseType::Int4),
            "bigserial" | "serial8" => Some(BaseType::Int8),
            _ => None,
        };
        let data_type = match serial_type {
            Some(base) => PgType::create(base),
            None => coercion::convert_ast_data_type(&column.data_type)?,
        };
        let mut nullable = true;
        let mut default = None;
        let mut default_sequence = None;
        let mut identity = None;
        for option in &column.options {
            match &option.option {
                ast::ColumnOption::Null => nullable = true,
                ast::ColumnOption::NotNull => nullable = false,
                ast::ColumnOption::Default(expr) => {
                    if serial_type.is_some() || identity.is_some() {
                        return Err(PgError::create(
                            SqlState::SyntaxError,
                            "multiple default values specified for column",
                        ));
                    }
                    default = Some(expr.clone());
                    default_sequence = resolve_default_sequence(&state.catalog, expr, persistence)?;
                }
                ast::ColumnOption::PrimaryKey(_) => {
                    let columns = vec![column_name.clone()];
                    constraints.push(crate::catalog::Constraint::PrimaryKey {
                        id: ConstraintId(0),
                        name: option
                            .name
                            .as_ref()
                            .map(normalize_identifier)
                            .unwrap_or_else(|| format!("{table_name}_pkey")),
                        columns,
                    });
                }
                ast::ColumnOption::Unique(_) => {
                    let columns = vec![column_name.clone()];
                    constraints.push(crate::catalog::Constraint::Unique {
                        id: ConstraintId(0),
                        name: option
                            .name
                            .as_ref()
                            .map(normalize_identifier)
                            .unwrap_or_else(|| format!("{table_name}_{column_name}_key")),
                        columns,
                    });
                }
                ast::ColumnOption::Check(check) => {
                    constraints.push(crate::catalog::Constraint::Check {
                        id: ConstraintId(0),
                        name: option
                            .name
                            .as_ref()
                            .map(normalize_identifier)
                            .unwrap_or_else(|| {
                                generate_constraint_name(
                                    format!("{table_name}_{column_name}_check"),
                                    &constraints,
                                )
                            }),
                        expression: check.expr.clone(),
                        validated: true,
                    })
                }
                ast::ColumnOption::ForeignKey(foreign_key) => {
                    let name = resolve_foreign_key_name(
                        option.name.as_ref(),
                        format!("{}_{}_fkey", table_name, column_name),
                    );
                    constraints.push(crate::catalog::Constraint::ForeignKey(ForeignKey {
                        id: ConstraintId(0),
                        name,
                        columns: vec![column_name.clone()],
                        foreign_table: crate::executor::normalize_relation_name(
                            &foreign_key.foreign_table,
                        )?,
                        foreign_table_id: TableId(0),
                        referred_columns: foreign_key
                            .referred_columns
                            .iter()
                            .map(normalize_identifier)
                            .collect(),
                        on_delete: convert_referential_action(foreign_key.on_delete),
                        on_update: convert_referential_action(foreign_key.on_update),
                        deferrable: foreign_key.characteristics.is_some_and(|characteristics| {
                            characteristics.deferrable.unwrap_or(false)
                        }),
                        initially_deferred: foreign_key.characteristics.is_some_and(
                            |characteristics| {
                                characteristics.initially == Some(ast::DeferrableInitial::Deferred)
                            },
                        ),
                        match_kind: foreign_key.match_kind,
                        validated: true,
                    }))
                }
                ast::ColumnOption::Generated {
                    generated_as,
                    sequence_options,
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
                    let kind = match generated_as {
                        ast::GeneratedAs::Always => IdentityKind::Always,
                        ast::GeneratedAs::ByDefault => IdentityKind::ByDefault,
                        ast::GeneratedAs::ExpStored => {
                            return Err(PgError::create(
                                SqlState::SyntaxError,
                                "invalid identity column declaration",
                            ));
                        }
                    };
                    if !matches!(
                        data_type.base,
                        BaseType::Int2 | BaseType::Int4 | BaseType::Int8
                    ) {
                        return Err(PgError::create(
                            SqlState::DatatypeMismatch,
                            "identity column type must be smallint, integer, or bigint",
                        ));
                    }
                    let sequence_name = create_generated_sequence_name(
                        &state.catalog,
                        &sequence_schemas,
                        &resolved_name,
                        &column_name,
                    );
                    let sequence = sequences::create_sequence_schema_for_type(
                        sequence_name.clone(),
                        data_type.base,
                        sequence_options.as_deref().unwrap_or(&[]),
                    )?;
                    sequence_schemas.push(sequence);
                    nullable = false;
                    default_sequence = Some(ResolvedRelationName {
                        schema_id: resolved_name.schema_id,
                        name: sequence_name,
                    });
                    identity = Some(kind);
                }
                option => {
                    return reject_unsupported(format!(
                        "column option is not implemented: {option}"
                    ));
                }
            }
        }
        if let Some(base) = serial_type {
            let sequence_name = create_generated_sequence_name(
                &state.catalog,
                &sequence_schemas,
                &resolved_name,
                &column_name,
            );
            let sequence =
                sequences::create_sequence_schema_for_type(sequence_name.clone(), base, &[])?;
            sequence_schemas.push(sequence);
            nullable = false;
            default_sequence = Some(ResolvedRelationName {
                schema_id: resolved_name.schema_id,
                name: sequence_name,
            });
        }
        let column = ColumnDef {
            name: column_name,
            data_type,
            nullable,
            default,
            default_sequence,
            identity,
        };
        validate_column_default(&column)?;
        columns.push(column);
    }
    for constraint in &create.constraints {
        match constraint {
            ast::TableConstraint::PrimaryKey(primary_key) => {
                let columns = primary_key
                    .columns
                    .iter()
                    .map(resolve_index_column_name)
                    .collect::<Result<Vec<_>>>()?;
                constraints.push(crate::catalog::Constraint::PrimaryKey {
                    id: ConstraintId(0),
                    name: primary_key
                        .name
                        .as_ref()
                        .map(normalize_identifier)
                        .unwrap_or_else(|| format!("{table_name}_pkey")),
                    columns,
                })
            }
            ast::TableConstraint::Unique(unique) => {
                let columns = unique
                    .columns
                    .iter()
                    .map(resolve_index_column_name)
                    .collect::<Result<Vec<_>>>()?;
                let default_name = format!("{table_name}_{}_key", columns.join("_"));
                constraints.push(crate::catalog::Constraint::Unique {
                    id: ConstraintId(0),
                    name: unique
                        .name
                        .as_ref()
                        .map(normalize_identifier)
                        .unwrap_or(default_name),
                    columns,
                })
            }
            ast::TableConstraint::Check(check) => {
                let base = find_first_referenced_column(&check.expr, &columns).map_or_else(
                    || format!("{table_name}_check"),
                    |column| format!("{table_name}_{column}_check"),
                );
                constraints.push(crate::catalog::Constraint::Check {
                    id: ConstraintId(0),
                    name: check
                        .name
                        .as_ref()
                        .map(normalize_identifier)
                        .unwrap_or_else(|| generate_constraint_name(base, &constraints)),
                    expression: check.expr.clone(),
                    validated: true,
                })
            }
            ast::TableConstraint::ForeignKey(foreign_key) => {
                let foreign_key_columns = foreign_key
                    .columns
                    .iter()
                    .map(normalize_identifier)
                    .collect::<Vec<_>>();
                let name = resolve_foreign_key_name(
                    foreign_key.name.as_ref(),
                    format!("{}_{}_fkey", table_name, foreign_key_columns.join("_")),
                );
                constraints.push(crate::catalog::Constraint::ForeignKey(ForeignKey {
                    id: ConstraintId(0),
                    name,
                    columns: foreign_key_columns,
                    foreign_table: crate::executor::normalize_relation_name(
                        &foreign_key.foreign_table,
                    )?,
                    foreign_table_id: TableId(0),
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
                    initially_deferred: foreign_key.characteristics.is_some_and(
                        |characteristics| {
                            characteristics.initially == Some(ast::DeferrableInitial::Deferred)
                        },
                    ),
                    match_kind: foreign_key.match_kind,
                    validated: true,
                }))
            }
            constraint => {
                return reject_unsupported(format!(
                    "table constraint is not implemented: {constraint}"
                ));
            }
        }
    }
    for constraint in &constraints {
        let (constraint_columns, primary_key) = match constraint {
            crate::catalog::Constraint::PrimaryKey { columns, .. } => (columns, true),
            crate::catalog::Constraint::Unique { columns, .. } => (columns, false),
            crate::catalog::Constraint::Check { .. }
            | crate::catalog::Constraint::ForeignKey(_) => continue,
        };
        for name in constraint_columns {
            let column = columns
                .iter_mut()
                .find(|column| column.name == *name)
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::UndefinedColumn,
                        format!("column {name:?} does not exist"),
                    )
                })?;
            validate_btree_key_type(column.data_type.base)?;
            if primary_key {
                column.nullable = false;
            }
        }
    }
    validate_check_constraint_types(&TableSchema {
        id: TableId(0),
        schema_id: resolved_name.schema_id,
        name: table_name.clone(),
        columns: columns.clone(),
        constraints: constraints.clone(),
        indexes: Vec::new(),
        triggers: Vec::new(),
        persistence,
    })?;
    let proposed = TableSchema {
        id: TableId(0),
        schema_id: resolved_name.schema_id,
        name: table_name.clone(),
        columns: columns.clone(),
        constraints: constraints.clone(),
        indexes: Vec::new(),
        triggers: Vec::new(),
        persistence,
    };
    validate_foreign_key_definitions(&state.catalog, &proposed)?;
    let id = state.catalog.create_named_table(
        resolved_name.clone(),
        columns,
        constraints,
        proposed.persistence,
    )?;
    let table = state
        .catalog
        .require_table_by_id(id)
        .expect("created table must exist")
        .clone();
    state.tables.insert(id, Table::create(table.clone()));
    for mut sequence in sequence_schemas {
        let column = table
            .columns
            .iter()
            .find(|column| {
                column.default_sequence.as_ref().is_some_and(|name| {
                    name.schema_id == resolved_name.schema_id && name.name == sequence.name
                })
            })
            .expect("generated sequence must belong to a table column");
        sequence.owned_by = Some((id, column.name.clone()));
        let initial = SequenceValueState {
            last_value: sequence.start_value,
            is_called: false,
        };
        let id = state.catalog.create_named_sequence(
            ResolvedRelationName {
                schema_id: resolved_name.schema_id,
                name: sequence.name.clone(),
            },
            sequence,
        )?;
        state
            .sequence_values
            .lock()
            .expect("sequence storage is poisoned")
            .insert(id, initial);
    }
    Ok(StatementResult::Affected(0))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_drop_tables(
    state: &mut DatabaseState,
    names: &[ast::ObjectName],
    if_exists: bool,
    cascade: bool,
    restrict: bool,
) -> Result<StatementResult> {
    if cascade || restrict {
        return reject_unsupported("DROP TABLE with CASCADE or RESTRICT is not implemented");
    }
    let mut table_names = Vec::new();
    let mut seen = BTreeSet::new();
    for object in names {
        let table_name = normalize_relation_name(object)?;
        match state.catalog.require_named_table(&table_name) {
            Ok(table) if seen.insert(table.id) => table_names.push(table_name),
            Ok(_) => {}
            Err(error) if if_exists && error.sqlstate == SqlState::UndefinedTable => {}
            Err(error) => return Err(error),
        }
    }
    for schema in state.catalog.drop_named_tables(&table_names)? {
        state.catalog.drop_owned_sequences(schema.id);
    }
    Ok(StatementResult::Affected(0))
}

pub(super) fn find_first_referenced_column(
    expression: &ast::Expr,
    columns: &[ColumnDef],
) -> Option<String> {
    let mut found = None;
    let _ = ast::visit_expressions(expression, |nested| {
        let name = match nested {
            ast::Expr::Identifier(identifier) => Some(normalize_identifier(identifier)),
            ast::Expr::CompoundIdentifier(identifiers) => {
                identifiers.last().map(normalize_identifier)
            }
            _ => None,
        };
        if let Some(name) = name
            && columns.iter().any(|column| column.name == name)
        {
            found = Some(name);
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    });
    found
}

pub(super) fn generate_constraint_name(
    base: String,
    constraints: &[crate::catalog::Constraint],
) -> String {
    let mut suffix = 0;
    loop {
        let name = if suffix == 0 {
            base.clone()
        } else {
            format!("{base}{suffix}")
        };
        if !constraints
            .iter()
            .any(|constraint| constraint.get_name() == Some(&name))
        {
            return name;
        }
        suffix += 1;
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn create_generated_sequence_name(
    catalog: &Catalog,
    sequences: &[SequenceSchema],
    table_name: &ResolvedRelationName,
    column_name: &str,
) -> String {
    let base = format!("{}_{column_name}_seq", table_name.name);
    let mut number = 0;
    loop {
        let name = if number == 0 {
            base.clone()
        } else {
            format!("{base}{number}")
        };
        if !catalog.has_resolved_relation(&ResolvedRelationName {
            schema_id: table_name.schema_id,
            name: name.clone(),
        }) && !sequences.iter().any(|sequence| sequence.name == name)
        {
            return name;
        }
        number += 1;
    }
}

pub(super) fn resolve_default_sequence(
    catalog: &Catalog,
    expression: &ast::Expr,
    persistence: TablePersistence,
) -> Result<Option<ResolvedRelationName>> {
    let Some(name) = extract_default_sequence_name(expression) else {
        let mut contains_sequence_call = false;
        let _ = ast::visit_expressions(expression, |nested| {
            let ast::Expr::Function(function) = nested else {
                return std::ops::ControlFlow::Continue(());
            };
            if normalize_unqualified_object_name(&function.name)
                .is_ok_and(|name| matches!(name.as_str(), "nextval" | "currval" | "setval"))
            {
                contains_sequence_call = true;
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        });
        if contains_sequence_call {
            return reject_unsupported("compound sequence defaults are not implemented");
        }
        return Ok(None);
    };
    let name = sequences::normalize_sequence_name(name)?;
    let sequence = catalog.require_named_sequence(&name).map_err(|error| {
        if error.sqlstate == SqlState::WrongObjectType {
            PgError::create(
                SqlState::FeatureNotSupported,
                "sequence defaults bound to non-sequence relations are not implemented",
            )
        } else {
            error
        }
    })?;
    let temporary_table = matches!(persistence, TablePersistence::Temporary { .. });
    let temporary_sequence = catalog.get_schema_name(sequence.schema_id) == TEMP_SCHEMA;
    if temporary_table != temporary_sequence {
        return reject_unsupported("cross-persistence sequence defaults are not implemented");
    }
    Ok(Some(ResolvedRelationName {
        schema_id: sequence.schema_id,
        name: sequence.name.clone(),
    }))
}

fn extract_default_sequence_name(expression: &ast::Expr) -> Option<&str> {
    match expression {
        ast::Expr::Nested(expr) => extract_default_sequence_name(expr),
        ast::Expr::Function(function)
            if normalize_unqualified_object_name(&function.name).as_deref() == Ok("nextval") =>
        {
            let ast::FunctionArguments::List(arguments) = &function.args else {
                return None;
            };
            let [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))] =
                arguments.args.as_slice()
            else {
                return None;
            };
            extract_default_sequence_literal(argument)
        }
        _ => None,
    }
}

fn extract_default_sequence_literal(expression: &ast::Expr) -> Option<&str> {
    match expression {
        ast::Expr::Cast { expr, .. } | ast::Expr::Nested(expr) => {
            extract_default_sequence_literal(expr)
        }
        ast::Expr::Value(value) => match &value.value {
            ast::Value::SingleQuotedString(value) => Some(value),
            _ => None,
        },
        _ => None,
    }
}
