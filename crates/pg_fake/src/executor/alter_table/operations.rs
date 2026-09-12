use super::{
    RewrittenRow,
    columns::{alter_column, create_column_definition},
    constraints::{create_foreign_key_constraint, create_table_constraint},
    dependencies::{
        ForeignKeyTarget, remove_column_dependencies, remove_local_referencing_foreign_keys,
        remove_referencing_foreign_keys, rename_local_constraint_columns,
        rename_schema_expressions,
    },
};
use crate::executor::{
    DatabaseState, StatementContext,
    column_defaults::{evaluate_column_default, validate_column_default},
    normalize_identifier, normalize_relation_name,
    row_constraints::validate_check_constraint_types,
    table_ddl::generate_constraint_name,
    validate_btree_key_type, views,
};
use crate::{
    catalog::TableSchema,
    error::{PgError, Result, SqlState, reject_unsupported},
};
use sqlparser::ast;
use std::collections::BTreeSet;

pub(super) fn apply_table_operation(
    state: &mut DatabaseState,
    schema: &mut TableSchema,
    rows: &mut [RewrittenRow],
    operation: &ast::AlterTableOperation,
    context: &StatementContext,
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
            let mut column = create_column_definition(state, schema, column_def)?;
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
                        validate_btree_key_type(column.data_type.base)?;
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
                        validate_btree_key_type(column.data_type.base)?;
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
            views::rename_column_references(&mut state.catalog, schema, &old_name, &new_name);
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
            views::rename_table_references(&mut state.catalog, schema.id, &new_name.name);
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
                let dependency = ForeignKeyTarget::Constraint {
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
