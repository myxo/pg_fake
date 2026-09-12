use std::collections::BTreeSet;

use sqlparser::ast;

use crate::{
    catalog::ResolvedRelationName,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{self, DatabaseState},
    parser,
    txn::RelationLockMode,
};

use super::super::catalog_dependencies::{
    CatalogDependency, collect_catalog_dependencies, extract_sequence_name,
};
use super::{ddl::collect_ddl_relation_locks, foreign_keys::collect_foreign_key_relation_locks};

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::session) fn collect_relation_locks(
    state: &DatabaseState,
    statement: &ast::Statement,
    prepared_dependencies: Option<&[CatalogDependency]>,
) -> Result<Vec<(String, RelationLockMode)>> {
    if let ast::Statement::Lock(lock) = statement {
        if lock.nowait {
            return reject_unsupported("LOCK TABLE NOWAIT is not implemented");
        }
        let mode = match lock
            .lock_mode
            .as_ref()
            .unwrap_or(&ast::LockTableMode::AccessExclusive)
        {
            ast::LockTableMode::Exclusive => RelationLockMode::TableExclusive,
            ast::LockTableMode::AccessExclusive => RelationLockMode::Exclusive,
            _ => return reject_unsupported("LOCK TABLE mode is not implemented"),
        };
        let mut locks = Vec::new();
        let mut seen = BTreeSet::new();
        for target in &lock.tables {
            if target.only || target.has_asterisk {
                return reject_unsupported("LOCK TABLE inheritance targets are not implemented");
            }
            let name = executor::normalize_relation_name(&target.name)?;
            let table = state.catalog.require_named_table(&name)?;
            if seen.insert(table.id) {
                locks.push((
                    table.id,
                    ResolvedRelationName {
                        schema_id: table.schema_id,
                        name: table.name.clone(),
                    }
                    .get_lock_name(),
                ));
            }
        }
        locks.sort_by_key(|(table_id, _)| *table_id);
        return Ok(locks.into_iter().map(|(_, name)| (name, mode)).collect());
    }
    if matches!(parser::classify(statement), parser::StatementKind::Ddl) {
        return collect_ddl_relation_locks(&state.catalog, statement);
    }
    let (expanded_statement, mutations) = executor::expand_ctes_for_analysis(statement, state)?;
    let locking_read = matches!(
        expanded_statement.as_ref(),
        ast::Statement::Query(query) if !query.locks.is_empty()
    );
    let discovered_dependencies;
    let dependencies = match prepared_dependencies {
        Some(dependencies) => dependencies,
        None => {
            discovered_dependencies = collect_catalog_dependencies(
                &state.catalog,
                std::iter::once(expanded_statement.as_ref()).chain(mutations.iter()),
            )?;
            &discovered_dependencies
        }
    };
    let mut locks = std::collections::BTreeMap::new();
    for dependency in dependencies {
        match dependency {
            CatalogDependency::Table { schema: table, .. } => {
                locks.insert(
                    ResolvedRelationName {
                        schema_id: table.schema_id,
                        name: table.name.clone(),
                    }
                    .get_lock_name(),
                    if locking_read {
                        RelationLockMode::RowShare
                    } else {
                        RelationLockMode::Shared
                    },
                );
            }
            CatalogDependency::Sequence {
                schema: sequence, ..
            } => {
                locks.insert(
                    ResolvedRelationName {
                        schema_id: sequence.schema_id,
                        name: sequence.name.clone(),
                    }
                    .get_lock_name(),
                    RelationLockMode::Shared,
                );
            }
            CatalogDependency::View { schema: view, .. } => {
                locks.insert(
                    ResolvedRelationName {
                        schema_id: view.schema_id,
                        name: view.name.clone(),
                    }
                    .get_lock_name(),
                    RelationLockMode::Shared,
                );
            }
            CatalogDependency::Constraint { .. } => {}
        }
    }
    collect_foreign_key_relation_locks(
        state,
        std::iter::once(expanded_statement.as_ref()).chain(mutations.iter()),
        &mut locks,
    )?;
    for mutation in std::iter::once(expanded_statement.as_ref()).chain(mutations.iter()) {
        let name = match mutation {
            ast::Statement::Insert(insert) => {
                Some(executor::resolve_insert_table_name(&insert.table)?)
            }
            ast::Statement::Update(update) => match &update.table.relation {
                ast::TableFactor::Table { name, .. } => {
                    Some(executor::normalize_relation_name(name)?)
                }
                _ => None,
            },
            ast::Statement::Delete(delete) => match &delete.from {
                ast::FromTable::WithFromKeyword(from) => from
                    .first()
                    .and_then(|table| {
                        let ast::TableFactor::Table { name, .. } = &table.relation else {
                            return None;
                        };
                        Some(executor::normalize_relation_name(name))
                    })
                    .transpose()?,
                _ => None,
            },
            _ => None,
        };
        let Some(name) = name else {
            continue;
        };
        if let Ok(table) = state.catalog.require_named_table(&name) {
            for trigger in &table.triggers {
                let function = state.catalog.require_function_by_id(trigger.function_id)?;
                locks.insert(
                    format!("function:{}:{}", function.schema_id.0, function.name),
                    RelationLockMode::Shared,
                );
            }
            locks
                .entry(
                    ResolvedRelationName {
                        schema_id: table.schema_id,
                        name: table.name.clone(),
                    }
                    .get_lock_name(),
                )
                .and_modify(|mode| *mode = (*mode).max(RelationLockMode::RowExclusive))
                .or_insert(RelationLockMode::RowExclusive);
        }
    }
    let mut sequence_error = None;
    let _ = ast::visit_expressions(statement, |expression| -> std::ops::ControlFlow<()> {
        let ast::Expr::Function(function) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(name) = executor::normalize_unqualified_object_name(&function.name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if !matches!(name.as_str(), "nextval" | "currval" | "setval") {
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
            Ok(name) => match state.catalog.resolve_relation_name(&name) {
                Ok(name) => {
                    locks.insert(name.get_lock_name(), RelationLockMode::Shared);
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
