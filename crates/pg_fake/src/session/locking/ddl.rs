use sqlparser::ast;

use crate::{
    catalog::{ResolvedRelationName, TEMP_SCHEMA, TablePersistence, ViewDependency},
    error::{PgError, Result, SqlState},
    executor,
    txn::RelationLockMode,
};

use super::super::catalog_dependencies::{
    CatalogDependency, collect_catalog_dependencies, extract_sequence_name,
};

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn collect_ddl_relation_locks(
    catalog: &crate::catalog::Catalog,
    statement: &ast::Statement,
) -> Result<Vec<(String, RelationLockMode)>> {
    let mut locks = std::collections::BTreeMap::new();
    match statement {
        ast::Statement::CreateFunction(create) => {
            let name =
                catalog.resolve_function_name(&executor::normalize_relation_name(&create.name)?)?;
            locks.insert(
                format!("function:{}:{}", name.schema_id.0, name.name),
                RelationLockMode::Exclusive,
            );
        }
        ast::Statement::DropFunction(drop) => {
            for description in &drop.func_desc {
                let name = executor::normalize_relation_name(&description.name)?;
                let resolved = catalog.resolve_function_name(&name)?;
                locks.insert(
                    format!("function:{}:{}", resolved.schema_id.0, resolved.name),
                    RelationLockMode::Exclusive,
                );
                if let Ok(function) = catalog.require_named_function(&name) {
                    for table in catalog.iterate_tables().filter(|table| {
                        table
                            .triggers
                            .iter()
                            .any(|trigger| trigger.function_id == function.id)
                    }) {
                        locks.insert(
                            ResolvedRelationName {
                                schema_id: table.schema_id,
                                name: table.name.clone(),
                            }
                            .get_lock_name(),
                            RelationLockMode::Exclusive,
                        );
                    }
                }
            }
        }
        ast::Statement::CreateTable(create) => {
            let relation_name = executor::normalize_relation_name(&create.name)?;
            let temporary =
                create.temporary || relation_name.schema.as_deref() == Some(TEMP_SCHEMA);
            let table_name = catalog.resolve_creation_name(&relation_name, temporary)?;
            locks.insert(table_name.get_lock_name(), RelationLockMode::Exclusive);
            let mut generated_sequences = Vec::new();
            for column in &create.columns {
                let column_name = executor::normalize_identifier(&column.name);
                let serial = matches!(
                    column.data_type.to_string().to_ascii_lowercase().as_str(),
                    "smallserial" | "serial2" | "serial" | "serial4" | "bigserial" | "serial8"
                );
                let identity = column
                    .options
                    .iter()
                    .any(|option| matches!(option.option, ast::ColumnOption::Generated { .. }));
                if serial || identity {
                    let base = format!("{}_{column_name}_seq", table_name.name);
                    let mut number = 0;
                    loop {
                        let candidate = if number == 0 {
                            base.clone()
                        } else {
                            format!("{base}{number}")
                        };
                        if !catalog.has_resolved_relation(&ResolvedRelationName {
                            schema_id: table_name.schema_id,
                            name: candidate.clone(),
                        }) && !generated_sequences.contains(&candidate)
                        {
                            locks.insert(
                                ResolvedRelationName {
                                    schema_id: table_name.schema_id,
                                    name: candidate.clone(),
                                }
                                .get_lock_name(),
                                RelationLockMode::Exclusive,
                            );
                            generated_sequences.push(candidate);
                            break;
                        }
                        number += 1;
                    }
                }
                for option in &column.options {
                    if let ast::ColumnOption::ForeignKey(foreign_key) = &option.option {
                        let name = catalog.resolve_relation_name(
                            &executor::normalize_relation_name(&foreign_key.foreign_table)?,
                        )?;
                        locks
                            .entry(name.get_lock_name())
                            .and_modify(|mode| *mode = (*mode).max(RelationLockMode::RowShare))
                            .or_insert(RelationLockMode::RowShare);
                    }
                }
            }
            for constraint in &create.constraints {
                if let ast::TableConstraint::ForeignKey(foreign_key) = constraint {
                    let name = catalog.resolve_relation_name(
                        &executor::normalize_relation_name(&foreign_key.foreign_table)?,
                    )?;
                    locks
                        .entry(name.get_lock_name())
                        .and_modify(|mode| *mode = (*mode).max(RelationLockMode::RowShare))
                        .or_insert(RelationLockMode::RowShare);
                }
            }
        }
        ast::Statement::CreateSequence {
            temporary,
            name,
            owned_by,
            ..
        } => {
            let relation_name = executor::normalize_relation_name(name)?;
            let temporary = *temporary || relation_name.schema.as_deref() == Some(TEMP_SCHEMA);
            locks.insert(
                catalog
                    .resolve_creation_name(&relation_name, temporary)?
                    .get_lock_name(),
                RelationLockMode::Exclusive,
            );
            if let Some(owned_by) = owned_by
                && matches!(owned_by.0.len(), 2 | 3)
            {
                let table = ast::ObjectName(owned_by.0[..owned_by.0.len() - 1].to_vec());
                locks
                    .entry(
                        catalog
                            .resolve_relation_name(&executor::normalize_relation_name(&table)?)?
                            .get_lock_name(),
                    )
                    .or_insert(RelationLockMode::Shared);
            }
        }
        ast::Statement::Drop {
            object_type: ast::ObjectType::Table,
            names: objects,
            ..
        } => {
            for object in objects {
                let name = executor::normalize_relation_name(object)?;
                locks.insert(
                    catalog.resolve_relation_name(&name)?.get_lock_name(),
                    RelationLockMode::Exclusive,
                );
                if let Ok(table) = catalog.require_named_table(&name) {
                    for (referencing, _) in catalog.collect_referencing_foreign_keys(table.id) {
                        locks
                            .entry(
                                ResolvedRelationName {
                                    schema_id: referencing.schema_id,
                                    name: referencing.name,
                                }
                                .get_lock_name(),
                            )
                            .or_insert(RelationLockMode::Shared);
                    }
                    for sequence in catalog.iterate_sequences().filter(|sequence| {
                        sequence.owned_by.as_ref().map(|(owner, _)| *owner) == Some(table.id)
                    }) {
                        locks.insert(
                            ResolvedRelationName {
                                schema_id: sequence.schema_id,
                                name: sequence.name.clone(),
                            }
                            .get_lock_name(),
                            RelationLockMode::Exclusive,
                        );
                    }
                    for sequence in table
                        .columns
                        .iter()
                        .filter_map(|column| column.default_sequence.as_ref())
                    {
                        locks
                            .entry(sequence.get_lock_name())
                            .or_insert(RelationLockMode::Shared);
                    }
                }
            }
        }
        ast::Statement::AlterTable(alter) => {
            let name = executor::normalize_relation_name(&alter.name)?;
            let table = match catalog.require_named_table(&name) {
                Ok(table) => table,
                Err(error) if alter.if_exists && error.sqlstate == SqlState::UndefinedTable => {
                    return Ok(Vec::new());
                }
                Err(error) => return Err(error),
            };
            locks.insert(
                ResolvedRelationName {
                    schema_id: table.schema_id,
                    name: table.name.clone(),
                }
                .get_lock_name(),
                RelationLockMode::Exclusive,
            );
            if alter.operations.iter().any(|operation| {
                matches!(
                    operation,
                    ast::AlterTableOperation::RenameColumn { .. }
                        | ast::AlterTableOperation::RenameTable { .. }
                        | ast::AlterTableOperation::DropColumn { .. }
                )
            }) {
                for view in catalog
                    .iterate_views()
                    .filter(|view| view.dependencies.contains(&ViewDependency::Table(table.id)))
                {
                    locks.insert(
                        ResolvedRelationName {
                            schema_id: view.schema_id,
                            name: view.name.clone(),
                        }
                        .get_lock_name(),
                        RelationLockMode::Exclusive,
                    );
                }
            }
            for operation in &alter.operations {
                if let ast::AlterTableOperation::AddColumn { column_def, .. } = operation {
                    for option in &column_def.options {
                        if let ast::ColumnOption::ForeignKey(foreign_key) = &option.option {
                            let parent = catalog.resolve_relation_name(
                                &executor::normalize_relation_name(&foreign_key.foreign_table)?,
                            )?;
                            locks
                                .entry(parent.get_lock_name())
                                .or_insert(RelationLockMode::Shared);
                        }
                    }
                    let serial = matches!(
                        column_def
                            .data_type
                            .to_string()
                            .to_ascii_lowercase()
                            .as_str(),
                        "smallserial" | "serial2" | "serial" | "serial4" | "bigserial" | "serial8"
                    );
                    let identity = column_def
                        .options
                        .iter()
                        .any(|option| matches!(option.option, ast::ColumnOption::Generated { .. }));
                    if serial || identity {
                        let column_name = executor::normalize_identifier(&column_def.name);
                        let base = format!("{}_{column_name}_seq", table.name);
                        let mut number = 0;
                        loop {
                            let candidate = if number == 0 {
                                base.clone()
                            } else {
                                format!("{base}{number}")
                            };
                            let candidate = ResolvedRelationName {
                                schema_id: table.schema_id,
                                name: candidate,
                            };
                            if !catalog.has_resolved_relation(&candidate) {
                                locks
                                    .insert(candidate.get_lock_name(), RelationLockMode::Exclusive);
                                break;
                            }
                            number += 1;
                        }
                    }
                }
                if let ast::AlterTableOperation::AddConstraint {
                    constraint: ast::TableConstraint::ForeignKey(foreign_key),
                    ..
                } = operation
                {
                    let parent = catalog.resolve_relation_name(
                        &executor::normalize_relation_name(&foreign_key.foreign_table)?,
                    )?;
                    locks
                        .entry(parent.get_lock_name())
                        .or_insert(RelationLockMode::Shared);
                }
                if let ast::AlterTableOperation::RenameTable { table_name } = operation {
                    let target = match table_name {
                        ast::RenameTableNameKind::To(name) | ast::RenameTableNameKind::As(name) => {
                            name
                        }
                    };
                    let target = executor::normalize_relation_name(target)?;
                    let temporary = matches!(table.persistence, TablePersistence::Temporary { .. });
                    let resolved = catalog.resolve_creation_name(&target, temporary)?;
                    locks.insert(resolved.get_lock_name(), RelationLockMode::Exclusive);
                }
                if matches!(
                    operation,
                    ast::AlterTableOperation::DropColumn {
                        drop_behavior: Some(ast::DropBehavior::Cascade),
                        ..
                    } | ast::AlterTableOperation::DropConstraint {
                        drop_behavior: Some(ast::DropBehavior::Cascade),
                        ..
                    }
                ) {
                    for (referencing, _) in catalog.collect_referencing_foreign_keys(table.id) {
                        locks.insert(
                            ResolvedRelationName {
                                schema_id: referencing.schema_id,
                                name: referencing.name,
                            }
                            .get_lock_name(),
                            RelationLockMode::Exclusive,
                        );
                    }
                }
            }
        }
        ast::Statement::CreateIndex(create) => {
            let table_name = executor::normalize_relation_name(&create.table_name)?;
            let table = catalog.require_named_table(&table_name)?;
            locks.insert(
                ResolvedRelationName {
                    schema_id: table.schema_id,
                    name: table.name.clone(),
                }
                .get_lock_name(),
                RelationLockMode::Exclusive,
            );
            let Some(name) = &create.name else {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "index name is required",
                ));
            };
            let name = executor::normalize_relation_name(name)?;
            let schema_id = match name.schema.as_deref() {
                Some(schema) => catalog.require_schema(schema)?.id,
                None => table.schema_id,
            };
            locks.insert(
                ResolvedRelationName {
                    schema_id,
                    name: name.name,
                }
                .get_lock_name(),
                RelationLockMode::Exclusive,
            );
        }
        ast::Statement::AlterIndex {
            if_exists,
            name,
            operation,
        } => {
            let name = executor::normalize_relation_name(name)?;
            match catalog.require_named_index(&name) {
                Ok((table, index)) => {
                    locks.insert(
                        ResolvedRelationName {
                            schema_id: table.schema_id,
                            name: table.name.clone(),
                        }
                        .get_lock_name(),
                        RelationLockMode::Exclusive,
                    );
                    locks.insert(
                        ResolvedRelationName {
                            schema_id: table.schema_id,
                            name: index.name.clone(),
                        }
                        .get_lock_name(),
                        RelationLockMode::Exclusive,
                    );
                    let ast::AlterIndexOperation::RenameIndex { index_name } = operation;
                    let target_name = executor::normalize_relation_name(index_name)?;
                    if target_name.schema.is_none() {
                        locks.insert(
                            ResolvedRelationName {
                                schema_id: table.schema_id,
                                name: target_name.name,
                            }
                            .get_lock_name(),
                            RelationLockMode::Exclusive,
                        );
                    }
                }
                Err(error) if *if_exists && error.sqlstate == SqlState::UndefinedObject => {}
                Err(error) => return Err(error),
            }
        }
        ast::Statement::Drop {
            object_type: ast::ObjectType::Index,
            names,
            if_exists,
            ..
        } => {
            for name in names {
                let name = executor::normalize_relation_name(name)?;
                match catalog.require_named_index(&name) {
                    Ok((table, index)) => {
                        locks.insert(
                            ResolvedRelationName {
                                schema_id: table.schema_id,
                                name: table.name.clone(),
                            }
                            .get_lock_name(),
                            RelationLockMode::Exclusive,
                        );
                        locks.insert(
                            ResolvedRelationName {
                                schema_id: table.schema_id,
                                name: index.name.clone(),
                            }
                            .get_lock_name(),
                            RelationLockMode::Exclusive,
                        );
                    }
                    Err(error) if *if_exists && error.sqlstate == SqlState::UndefinedObject => {}
                    Err(error) => return Err(error),
                }
            }
        }
        ast::Statement::CreateView(create) => {
            let name = executor::normalize_relation_name(&create.name)?;
            let temporary = create.temporary || name.schema.as_deref() == Some(TEMP_SCHEMA);
            locks.insert(
                catalog
                    .resolve_creation_name(&name, temporary)?
                    .get_lock_name(),
                RelationLockMode::Exclusive,
            );
            for dependency in collect_catalog_dependencies(
                catalog,
                &[ast::Statement::Query(create.query.clone())],
            )? {
                match dependency {
                    CatalogDependency::Table { schema, .. } => {
                        locks
                            .entry(
                                ResolvedRelationName {
                                    schema_id: schema.schema_id,
                                    name: schema.name,
                                }
                                .get_lock_name(),
                            )
                            .or_insert(RelationLockMode::Shared);
                    }
                    CatalogDependency::View { schema, .. } => {
                        locks
                            .entry(
                                ResolvedRelationName {
                                    schema_id: schema.schema_id,
                                    name: schema.name,
                                }
                                .get_lock_name(),
                            )
                            .or_insert(RelationLockMode::Shared);
                    }
                    CatalogDependency::Sequence { .. } | CatalogDependency::Constraint { .. } => {}
                }
            }
        }
        ast::Statement::CreateTrigger(create) => {
            let table = catalog
                .require_named_table(&executor::normalize_relation_name(&create.table_name)?)?;
            locks.insert(
                ResolvedRelationName {
                    schema_id: table.schema_id,
                    name: table.name.clone(),
                }
                .get_lock_name(),
                RelationLockMode::Exclusive,
            );
            if let Some(body) = &create.exec_body {
                let name = executor::normalize_relation_name(&body.func_desc.name)?;
                let resolved = catalog.resolve_function_name(&name)?;
                locks.insert(
                    format!("function:{}:{}", resolved.schema_id.0, resolved.name),
                    RelationLockMode::Shared,
                );
            }
        }
        ast::Statement::DropTrigger(drop) => {
            if let Some(table_name) = &drop.table_name {
                let table_name = executor::normalize_relation_name(table_name)?;
                let table = match catalog.require_named_table(&table_name) {
                    Ok(table) => table,
                    Err(error) if drop.if_exists && error.sqlstate == SqlState::UndefinedTable => {
                        return Ok(Vec::new());
                    }
                    Err(error) => return Err(error),
                };
                locks.insert(
                    ResolvedRelationName {
                        schema_id: table.schema_id,
                        name: table.name.clone(),
                    }
                    .get_lock_name(),
                    RelationLockMode::Exclusive,
                );
            }
        }
        ast::Statement::AlterTrigger { table_name, .. } => {
            let table =
                catalog.require_named_table(&executor::normalize_relation_name(table_name)?)?;
            locks.insert(
                ResolvedRelationName {
                    schema_id: table.schema_id,
                    name: table.name.clone(),
                }
                .get_lock_name(),
                RelationLockMode::Exclusive,
            );
        }
        ast::Statement::Comment {
            object_type: ast::CommentObject::View,
            object_name,
            ..
        } => {
            let view =
                catalog.require_named_view(&executor::normalize_relation_name(object_name)?)?;
            locks.insert(
                ResolvedRelationName {
                    schema_id: view.schema_id,
                    name: view.name.clone(),
                }
                .get_lock_name(),
                RelationLockMode::Exclusive,
            );
        }
        ast::Statement::Drop {
            object_type: ast::ObjectType::View,
            names,
            ..
        } => {
            for name in names {
                locks.insert(
                    catalog
                        .resolve_relation_name(&executor::normalize_relation_name(name)?)?
                        .get_lock_name(),
                    RelationLockMode::Exclusive,
                );
            }
        }
        ast::Statement::Drop {
            object_type: ast::ObjectType::Sequence,
            names: objects,
            ..
        } => {
            for object in objects {
                locks.insert(
                    catalog
                        .resolve_relation_name(&executor::normalize_relation_name(object)?)?
                        .get_lock_name(),
                    RelationLockMode::Exclusive,
                );
            }
        }
        _ => {}
    }
    let mut sequence_error = None;
    let _ = ast::visit_expressions(statement, |expression| -> std::ops::ControlFlow<()> {
        let ast::Expr::Function(function) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(function_name) = executor::normalize_function_name(&function.name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if !matches!(function_name.as_str(), "nextval" | "currval" | "setval") {
            return std::ops::ControlFlow::Continue(());
        }
        let ast::FunctionArguments::List(arguments) = &function.args else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))) =
            arguments.args.first()
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(name) = extract_sequence_name(argument) else {
            sequence_error = Some(PgError::create(
                SqlState::FeatureNotSupported,
                "computed sequence names are not implemented",
            ));
            return std::ops::ControlFlow::Break(());
        };
        match executor::normalize_sequence_name(name) {
            Ok(name) => match catalog.resolve_relation_name(&name) {
                Ok(name) => {
                    locks
                        .entry(name.get_lock_name())
                        .or_insert(RelationLockMode::Shared);
                    std::ops::ControlFlow::Continue(())
                }
                Err(error) => {
                    sequence_error = Some(error);
                    std::ops::ControlFlow::Break(())
                }
            },
            Err(error) => {
                sequence_error = Some(error);
                std::ops::ControlFlow::Break(())
            }
        }
    });
    if let Some(error) = sequence_error {
        return Err(error);
    }
    Ok(locks.into_iter().collect())
}
