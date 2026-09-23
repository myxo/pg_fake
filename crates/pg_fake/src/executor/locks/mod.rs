use crate::executor::{
    DatabaseState, PreparedMutationTarget, StatementContext,
    expressions::is_null_literal,
    from::resolve_unique_point_lookup,
    normalize_relation_name, prepared, query, resolve_insert_table_name,
    scope::{RowScope, bind_target_scope},
    writes,
};
use crate::{
    error::{PgError, Result, SqlState},
    txn::{RowLockKey, RowLockMode, Snapshot, TransactionStatus, Xid, find_visible_version},
    value::{BaseType, Value},
};
use sqlparser::ast::{self, Spanned as _};

mod ctes;
mod foreign_keys;
mod insert;

pub(crate) use ctes::collect_required_cte_row_locks;
pub(super) use foreign_keys::collect_foreign_key_locks_for_rows;
use foreign_keys::collect_insert_foreign_key_locks;
use insert::{collect_insert_conflict_locks, collect_triggered_insert_locks};

#[derive(Clone)]
pub(crate) struct RequiredRowLock {
    pub(crate) key: RowLockKey,
    pub(crate) mode: RowLockMode,
    pub(crate) mutation_candidate: Option<MutationCandidate>,
}

#[derive(Clone)]
pub(crate) struct MutationCandidate {
    pub(crate) version_xmin: Xid,
    pub(crate) row: Option<Vec<Value>>,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn collect_required_row_locks(
    state: &DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Vec<RequiredRowLock>> {
    if let ast::Statement::Insert(insert) = statement {
        let name = resolve_insert_table_name(&insert.table)?;
        if state.catalog.require_named_view(&name).is_ok() {
            return Ok(Vec::new());
        }
        let schema = state.catalog.require_named_table(&name)?;
        let source_locks = insert
            .source
            .as_ref()
            .map(|source| super::views::expand_query_views(&state.catalog, source))
            .transpose()?
            .flatten()
            .is_some_and(|source| query::contains_locking_operations(&source));
        if !schema.triggers.is_empty()
            || schema.columns.iter().any(|column| {
                column
                    .default
                    .as_ref()
                    .is_some_and(query::contains_locking_operations)
            })
            || source_locks
            || query::contains_locking_operations(insert)
            || matches!(
                insert.on,
                Some(ast::OnInsert::OnConflict(ast::OnConflict {
                    action: ast::OnConflictAction::DoUpdate(_),
                    ..
                }))
            )
        {
            return collect_triggered_insert_locks(state, insert, schema, xid, snapshot, context);
        }
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
            let name = normalize_relation_name(table_name)?;
            if state.catalog.require_named_view(&name).is_ok() {
                return Ok(Vec::new());
            }
            if update.from.is_some() {
                return writes::collect_update_cte_locks(state, update, xid, snapshot, context);
            }
            (
                state.catalog.require_named_table(&name)?,
                alias.as_ref().map(|alias| &alias.name),
                update
                    .from
                    .is_none()
                    .then_some(update.selection.as_ref())
                    .flatten(),
                RowLockMode::NoKeyUpdate,
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
            let name = normalize_relation_name(table_name)?;
            if state.catalog.require_named_view(&name).is_ok() {
                return Ok(Vec::new());
            }
            if delete.using.is_some() {
                return writes::collect_delete_cte_locks(state, delete, xid, snapshot, context);
            }
            let schema = state.catalog.require_named_table(&name)?;
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
            let expanded = super::views::expand_query_views(&state.catalog, query)?;
            let query = expanded.as_ref().unwrap_or(query);
            if query::contains_locking_operations(query) {
                let mut invocation = context.clone();
                invocation.capture_lock_queries = true;
                query::execute_query(state, query, xid, snapshot, &invocation)?;
            }
            return Ok(std::mem::take(
                &mut *context
                    .select_row_locks
                    .lock()
                    .expect("select locks mutex is poisoned"),
            ));
        }
        _ => return Ok(Vec::new()),
    };
    let (schema, alias, selection, mode, retain_mutation_candidates, retain_mutation_row) = target;
    if let Some(selection) = selection {
        let base = query::infer_query_expression_type(
            state,
            selection,
            &bind_target_scope(schema, alias),
        )?
        .base;
        if base != BaseType::Bool && !is_null_literal(selection) {
            return Ok(Vec::new());
        }
    }
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    let prepared_targets = match statement {
        ast::Statement::Update(update) => {
            context.get_prepared_mutation_targets(update.span(), snapshot.commit_seq)
        }
        ast::Statement::Delete(delete) => {
            context.get_prepared_mutation_targets(delete.span(), snapshot.commit_seq)
        }
        _ => None,
    };
    let mut locks = if let Some(targets) = &prepared_targets {
        targets
            .iter()
            .map(|target| RequiredRowLock {
                key: RowLockKey {
                    table_id: schema.id,
                    row_id: target.row_id,
                },
                mode,
                mutation_candidate: Some(MutationCandidate {
                    version_xmin: target.version_xmin,
                    row: Some(target.current.clone()),
                }),
            })
            .collect()
    } else if let Some((column, value)) =
        resolve_unique_point_lookup(table, schema, selection, RowScope::Table(schema), context)?
    {
        if let Some(key) = table.create_unique_read_key(&[column], std::slice::from_ref(&value)) {
            state.record_read(
                xid,
                crate::serializable::Access::Unique(schema.id, vec![column], key),
            );
        }
        match table.find_unique_visible_version(
            &[column],
            &[value],
            snapshot,
            xid,
            &state.transactions,
        ) {
            Some((row_id, version)) => {
                state.record_read(xid, crate::serializable::Access::Row(schema.id, row_id));
                check_concurrent_update(state, version, xid, snapshot)?;
                vec![RequiredRowLock {
                    key: RowLockKey {
                        table_id: schema.id,
                        row_id,
                    },
                    mode,
                    mutation_candidate: retain_mutation_candidates.then(|| MutationCandidate {
                        version_xmin: version.xmin,
                        row: retain_mutation_row.then(|| version.row.clone()),
                    }),
                }]
            }
            None => Vec::new(),
        }
    } else {
        state.record_read(xid, crate::serializable::Access::Relation(schema.id));
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
                state.record_read(xid, crate::serializable::Access::Row(schema.id, row_id));
                if let Some(selection) = selection {
                    let value = if let Some(prepared_selection) = &prepared_selection {
                        prepared::evaluate_prepared_expression(
                            prepared_selection,
                            &version.row,
                            &[],
                            context.deadline,
                        )?
                    } else {
                        super::subqueries::evaluate_query_expression(
                            state,
                            selection,
                            &bound_scope,
                            &version.row,
                            xid,
                            snapshot,
                            context,
                        )?
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
            })?
    };
    if prepared_targets.is_none() && retain_mutation_candidates {
        let targets = locks
            .iter()
            .map(|required| {
                let candidate = required.mutation_candidate.as_ref()?;
                Some(PreparedMutationTarget {
                    row_id: required.key.row_id,
                    version_xmin: candidate.version_xmin,
                    current: candidate.row.clone()?,
                    bound_row: None,
                })
            })
            .collect::<Option<Vec<_>>>();
        if let Some(targets) = targets {
            match statement {
                ast::Statement::Update(update) => context.set_prepared_mutation_targets(
                    update.span(),
                    update.to_string(),
                    snapshot.commit_seq,
                    targets,
                ),
                ast::Statement::Delete(delete) => context.set_prepared_mutation_targets(
                    delete.span(),
                    delete.to_string(),
                    snapshot.commit_seq,
                    targets,
                ),
                _ => {}
            }
        }
    }
    if let ast::Statement::Update(update) = statement {
        if locks.iter().any(|required| {
            required.key.table_id == schema.id
                && !state.row_locks.is_held(required.key, xid, required.mode)
        }) {
            context.request_row_lock_recheck();
            return Ok(locks);
        }
        let rows = writes::prepare_update_rows(state, update, schema, xid, snapshot, context)?;
        for lock in &mut locks {
            if let Some(row) = rows.iter().find(|row| row.row_id == lock.key.row_id)
                && let Some(updated) = &row.updated
            {
                lock.mode = resolve_update_lock_mode(schema, &row.current, updated);
            }
        }
        locks.extend(collect_foreign_key_locks_for_rows(
            state,
            schema,
            rows.iter().filter_map(|row| row.updated.as_ref()),
            xid,
        )?);
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
pub(super) fn check_concurrent_update(
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

pub(super) fn resolve_update_lock_mode(
    schema: &crate::catalog::TableSchema,
    current: &[Value],
    updated: &[Value],
) -> RowLockMode {
    let keys = schema
        .constraints
        .iter()
        .filter_map(|constraint| match constraint {
            crate::catalog::Constraint::PrimaryKey { columns, .. }
            | crate::catalog::Constraint::Unique { columns, .. } => {
                Some(columns.iter().map(String::as_str).collect::<Vec<_>>())
            }
            _ => None,
        })
        .chain(
            schema
                .indexes
                .iter()
                .filter(|index| index.unique && index.predicate.is_none())
                .map(|index| {
                    index
                        .columns
                        .iter()
                        .map(|column| column.name.as_str())
                        .collect()
                }),
        );
    for columns in keys {
        for name in columns {
            let index = schema
                .columns
                .iter()
                .position(|column| column.name == name)
                .expect("index column exists");
            if current[index] != updated[index] {
                return RowLockMode::Update;
            }
        }
    }
    RowLockMode::NoKeyUpdate
}
