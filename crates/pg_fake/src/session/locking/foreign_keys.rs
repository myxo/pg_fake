use std::collections::BTreeSet;

use sqlparser::ast;

use crate::{
    catalog::{ResolvedRelationName, TableId},
    error::{Result, reject_unsupported},
    executor::{self, DatabaseState},
    txn::RelationLockMode,
};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ForeignKeyMutation {
    Delete(TableId),
    Update {
        table: TableId,
        columns: Vec<String>,
    },
}

fn collect_assignment_columns(assignments: &[ast::Assignment]) -> Result<Vec<String>> {
    let mut columns = BTreeSet::new();
    for assignment in assignments {
        let ast::AssignmentTarget::ColumnName(column) = &assignment.target else {
            return reject_unsupported("UPDATE tuple assignment is not implemented");
        };
        columns.insert(executor::normalize_unqualified_object_name(column)?);
    }
    Ok(columns.into_iter().collect())
}

pub(super) fn collect_foreign_key_relation_locks<'a>(
    state: &DatabaseState,
    statements: impl IntoIterator<Item = &'a ast::Statement>,
    locks: &mut std::collections::BTreeMap<String, RelationLockMode>,
) -> Result<()> {
    let mut pending = Vec::new();
    for statement in statements {
        match statement {
            ast::Statement::Insert(insert) => {
                let name = executor::resolve_insert_table_name(&insert.table)?;
                if state.catalog.require_named_view(&name).is_ok() {
                    continue;
                }
                let table = state.catalog.require_named_table(&name)?;
                for constraint in &table.constraints {
                    if let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint {
                        let parent = state
                            .catalog
                            .require_table_by_id(foreign_key.foreign_table_id)?;
                        locks
                            .entry(
                                ResolvedRelationName {
                                    schema_id: parent.schema_id,
                                    name: parent.name.clone(),
                                }
                                .get_lock_name(),
                            )
                            .and_modify(|mode| *mode = (*mode).max(RelationLockMode::RowShare))
                            .or_insert(RelationLockMode::RowShare);
                    }
                }
                if let Some(ast::OnInsert::OnConflict(ast::OnConflict {
                    action: ast::OnConflictAction::DoUpdate(update),
                    ..
                })) = &insert.on
                {
                    pending.push(ForeignKeyMutation::Update {
                        table: table.id,
                        columns: if table.triggers.is_empty() {
                            collect_assignment_columns(&update.assignments)?
                        } else {
                            table
                                .columns
                                .iter()
                                .map(|column| column.name.clone())
                                .collect()
                        },
                    });
                }
            }
            ast::Statement::Update(update) => {
                let ast::TableFactor::Table { name, .. } = &update.table.relation else {
                    continue;
                };
                let name = executor::normalize_relation_name(name)?;
                if state.catalog.require_named_view(&name).is_ok() {
                    continue;
                }
                let table = state.catalog.require_named_table(&name)?;
                pending.push(ForeignKeyMutation::Update {
                    table: table.id,
                    columns: if table.triggers.is_empty() {
                        collect_assignment_columns(&update.assignments)?
                    } else {
                        table
                            .columns
                            .iter()
                            .map(|column| column.name.clone())
                            .collect()
                    },
                });
            }
            ast::Statement::Delete(delete) => {
                let ast::FromTable::WithFromKeyword(from) = &delete.from else {
                    continue;
                };
                let Some(ast::TableWithJoins {
                    relation: ast::TableFactor::Table { name, .. },
                    ..
                }) = from.first()
                else {
                    continue;
                };
                let name = executor::normalize_relation_name(name)?;
                if state.catalog.require_named_view(&name).is_ok() {
                    continue;
                }
                let table = state.catalog.require_named_table(&name)?;
                pending.push(ForeignKeyMutation::Delete(table.id));
            }
            _ => {}
        }
    }
    let mut visited = BTreeSet::new();
    while let Some(mutation) = pending.pop() {
        if !visited.insert(mutation.clone()) {
            continue;
        }
        let (table_id, updated_columns) = match &mutation {
            ForeignKeyMutation::Delete(table) => (*table, None),
            ForeignKeyMutation::Update { table, columns } => (*table, Some(columns.as_slice())),
        };
        let table = state.catalog.require_table_by_id(table_id)?;
        if let Some(updated_columns) = updated_columns {
            for constraint in &table.constraints {
                let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
                    continue;
                };
                if foreign_key
                    .columns
                    .iter()
                    .any(|column| updated_columns.contains(column))
                {
                    let parent = state
                        .catalog
                        .require_table_by_id(foreign_key.foreign_table_id)?;
                    locks
                        .entry(
                            ResolvedRelationName {
                                schema_id: parent.schema_id,
                                name: parent.name.clone(),
                            }
                            .get_lock_name(),
                        )
                        .and_modify(|mode| *mode = (*mode).max(RelationLockMode::RowShare))
                        .or_insert(RelationLockMode::RowShare);
                }
            }
        }
        for (child, foreign_key) in state.catalog.collect_referencing_foreign_keys(table_id) {
            if let Some(updated_columns) = updated_columns {
                let referred_columns = if foreign_key.referred_columns.is_empty() {
                    table
                        .constraints
                        .iter()
                        .find_map(|constraint| match constraint {
                            crate::catalog::Constraint::PrimaryKey { columns, .. } => Some(columns),
                            _ => None,
                        })
                        .expect("foreign key definition was validated")
                } else {
                    &foreign_key.referred_columns
                };
                if !referred_columns
                    .iter()
                    .any(|column| updated_columns.contains(column))
                {
                    continue;
                }
            }
            let action = if updated_columns.is_some() {
                foreign_key.on_update
            } else {
                foreign_key.on_delete
            };
            if matches!(
                action,
                crate::catalog::ForeignKeyAction::Cascade
                    | crate::catalog::ForeignKeyAction::SetNull
                    | crate::catalog::ForeignKeyAction::SetDefault
            ) {
                for trigger in &child.triggers {
                    let function = state.catalog.require_function_by_id(trigger.function_id)?;
                    locks.insert(
                        format!("function:{}:{}", function.schema_id.0, function.name),
                        RelationLockMode::Shared,
                    );
                }
            }
            let mode = match action {
                crate::catalog::ForeignKeyAction::Cascade
                | crate::catalog::ForeignKeyAction::SetNull
                | crate::catalog::ForeignKeyAction::SetDefault => RelationLockMode::RowExclusive,
                crate::catalog::ForeignKeyAction::NoAction
                | crate::catalog::ForeignKeyAction::Restrict => RelationLockMode::RowShare,
            };
            locks
                .entry(
                    ResolvedRelationName {
                        schema_id: child.schema_id,
                        name: child.name.clone(),
                    }
                    .get_lock_name(),
                )
                .and_modify(|held| *held = (*held).max(mode))
                .or_insert(mode);
            match action {
                crate::catalog::ForeignKeyAction::Cascade if updated_columns.is_none() => {
                    pending.push(ForeignKeyMutation::Delete(child.id));
                }
                crate::catalog::ForeignKeyAction::Cascade
                | crate::catalog::ForeignKeyAction::SetNull
                | crate::catalog::ForeignKeyAction::SetDefault => {
                    pending.push(ForeignKeyMutation::Update {
                        table: child.id,
                        columns: if child.triggers.is_empty() {
                            foreign_key.columns
                        } else {
                            child
                                .columns
                                .iter()
                                .map(|column| column.name.clone())
                                .collect()
                        },
                    });
                }
                crate::catalog::ForeignKeyAction::NoAction
                | crate::catalog::ForeignKeyAction::Restrict => {}
            }
        }
    }
    Ok(())
}
