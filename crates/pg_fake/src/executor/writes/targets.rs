use super::update::prepare_update_rows;
use crate::executor::{
    DatabaseState, MutationCandidate, PreparedMutationTarget, RequiredRowLock, StatementContext,
    from, locks, normalize_relation_name,
    scope::{BoundScope, RowScope, bind_from_scope, bind_target_scope, combine_bound_scopes},
    subqueries,
};
use crate::{
    catalog::{Constraint, TableId, TableSchema},
    error::Result,
    storage::RowId,
    txn::{CommandId, RowLockKey, RowLockMode, Snapshot, Xid, find_visible_version},
    value::Value,
};
use sqlparser::ast::{self, Spanned as _};

pub(super) type MutationTarget = (RowId, Xid, Vec<Value>, Option<Vec<Value>>);

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn create_mutation_scope(
    state: &DatabaseState,
    schema: &TableSchema,
    alias: Option<&ast::Ident>,
    from: &[ast::TableWithJoins],
) -> Result<BoundScope> {
    Ok(combine_bound_scopes(
        bind_target_scope(schema, alias),
        bind_from_scope(&state.catalog, from)?,
    ))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn materialize_mutation_source_rows(
    state: &DatabaseState,
    from: &[ast::TableWithJoins],
    scope: &BoundScope,
    target_columns: usize,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Vec<Vec<Value>>> {
    if from.is_empty() {
        return Ok(vec![vec![Value::Null; scope.columns.len()]]);
    }
    from::materialize_from_rows(
        state,
        from,
        scope,
        target_columns,
        xid,
        snapshot,
        context,
        None,
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn matches_mutation_row(
    state: &DatabaseState,
    selection: Option<&ast::Expr>,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<bool> {
    let Some(selection) = selection else {
        return Ok(true);
    };
    Ok(
        match subqueries::evaluate_query_expression(
            state, selection, scope, row, xid, snapshot, context,
        )? {
            Value::Bool(value) => value,
            Value::Null => false,
            _ => unreachable!("WHERE expression was type-checked"),
        },
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn collect_mutation_targets(
    state: &DatabaseState,
    schema: &TableSchema,
    selection: Option<&ast::Expr>,
    scope: &BoundScope,
    source_rows: &[Vec<Value>],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
    mutation_targets: Option<Vec<RequiredRowLock>>,
) -> Result<Vec<MutationTarget>> {
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    let needs_bound_row = scope.columns.len() > schema.columns.len();
    if let Some(mutation_targets) = mutation_targets {
        let [source_row] = source_rows else {
            unreachable!("lock-selected mutations do not have source rows");
        };
        return mutation_targets
            .into_iter()
            .filter(|required| required.key.table_id == schema.id)
            .map(|required| {
                let candidate = required
                    .mutation_candidate
                    .expect("mutation target locks retain their selected row");
                let candidate_row = candidate
                    .row
                    .expect("row-consuming mutations retain their selected row");
                let bound_row = if needs_bound_row {
                    let mut row = source_row.clone();
                    row[..schema.columns.len()].clone_from_slice(&candidate_row);
                    Some(row)
                } else {
                    None
                };
                Ok((
                    required.key.row_id,
                    candidate.version_xmin,
                    candidate_row,
                    bound_row,
                ))
            })
            .collect();
    }
    if let [source_row] = source_rows
        && let Some((column, value)) = locks::resolve_unique_point_lookup(
            table,
            schema,
            selection,
            RowScope::Bound(scope),
            context,
        )?
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
        if version.xmax == Some(xid) && version.xmax_command_id == Some(context.command_id) {
            return Ok(Vec::new());
        }
        let mut row = source_row.clone();
        row[..schema.columns.len()].clone_from_slice(&version.row);
        if matches_mutation_row(state, selection, scope, &row, xid, snapshot, context)? {
            return Ok(vec![(
                row_id,
                version.xmin,
                version.row.clone(),
                needs_bound_row.then_some(row),
            )]);
        }
        return Ok(Vec::new());
    }
    table
        .iterate_version_chains()
        .try_fold(Vec::new(), |mut targets, (row_id, chain)| {
            let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions)
            else {
                return Ok(targets);
            };
            if version.xmax == Some(xid) && version.xmax_command_id == Some(context.command_id) {
                return Ok(targets);
            }
            for source_row in source_rows {
                let mut row = source_row.clone();
                row[..schema.columns.len()].clone_from_slice(&version.row);
                if matches_mutation_row(state, selection, scope, &row, xid, snapshot, context)? {
                    targets.push((
                        row_id,
                        version.xmin,
                        version.row.clone(),
                        needs_bound_row.then_some(row),
                    ));
                    break;
                }
            }
            Ok(targets)
        })
}

pub(super) fn has_mutated_target_in_command(
    state: &DatabaseState,
    table_id: TableId,
    row_id: RowId,
    version_xmin: Xid,
    xid: Xid,
    command_id: CommandId,
) -> bool {
    let table = state
        .tables
        .get(&table_id)
        .expect("catalog table must have storage");
    let chain = table
        .get_version_chain(row_id)
        .expect("prepared mutation row must exist");
    assert!(
        chain
            .versions
            .iter()
            .any(|version| version.xmin == version_xmin),
        "prepared mutation version must exist"
    );
    chain.versions.iter().any(|version| {
        version.xmin == version_xmin
            && version.xmax == Some(xid)
            && version.xmax_command_id == Some(command_id)
    })
}

fn validate_mutation_target_versions(
    state: &DatabaseState,
    schema: &TableSchema,
    targets: &[MutationTarget],
    xid: Xid,
    snapshot: &Snapshot,
) -> Result<()> {
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    for (row_id, version_xmin, _, _) in targets {
        let chain = table
            .get_version_chain(*row_id)
            .expect("selected mutation row must exist");
        let version = chain
            .versions
            .iter()
            .find(|version| version.xmin == *version_xmin)
            .expect("selected mutation version must exist");
        locks::check_concurrent_update(state, version, xid, snapshot)?;
    }
    Ok(())
}

pub(in crate::executor) fn collect_update_cte_locks(
    state: &DatabaseState,
    update: &ast::Update,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Vec<RequiredRowLock>> {
    let ast::TableFactor::Table {
        name: table_name,
        alias,
        args: None,
        ..
    } = &update.table.relation
    else {
        return Ok(Vec::new());
    };
    let schema = state
        .catalog
        .require_named_table(&normalize_relation_name(table_name)?)?;
    let from = match &update.from {
        None => &[][..],
        Some(ast::UpdateTableFromKind::AfterSet(from)) => from.as_slice(),
        Some(ast::UpdateTableFromKind::BeforeSet(_)) => return Ok(Vec::new()),
    };
    let scope =
        create_mutation_scope(state, schema, alias.as_ref().map(|alias| &alias.name), from)?;
    let occurrence = update.span();
    let sql = update.to_string();
    let targets = match context.get_prepared_mutation_targets(occurrence, snapshot.commit_seq) {
        Some(targets) => targets,
        None => {
            let source_rows = materialize_mutation_source_rows(
                state,
                from,
                &scope,
                schema.columns.len(),
                xid,
                snapshot,
                context,
            )?;
            let targets = collect_mutation_targets(
                state,
                schema,
                update.selection.as_ref(),
                &scope,
                &source_rows,
                xid,
                snapshot,
                context,
                None,
            )?;
            validate_mutation_target_versions(state, schema, &targets, xid, snapshot)?;
            let targets = targets
                .into_iter()
                .map(
                    |(row_id, version_xmin, current, bound_row)| PreparedMutationTarget {
                        row_id,
                        version_xmin,
                        current,
                        bound_row,
                    },
                )
                .collect::<Vec<_>>();
            context.set_prepared_mutation_targets(
                occurrence,
                sql,
                snapshot.commit_seq,
                targets.clone(),
            );
            targets
        }
    };
    let mut locks = targets
        .iter()
        .map(|target| RequiredRowLock {
            key: RowLockKey {
                table_id: schema.id,
                row_id: target.row_id,
            },
            mode: RowLockMode::Update,
            mutation_candidate: Some(MutationCandidate {
                version_xmin: target.version_xmin,
                row: Some(target.current.clone()),
            }),
        })
        .collect::<Vec<_>>();
    if locks
        .iter()
        .any(|lock| !state.row_locks.is_held(lock.key, xid, lock.mode))
    {
        context.request_row_lock_recheck_with_locks(locks.clone());
        return Ok(locks);
    }
    if schema
        .constraints
        .iter()
        .any(|constraint| matches!(constraint, Constraint::ForeignKey(_)))
    {
        let prepared = prepare_update_rows(state, update, schema, xid, snapshot, context)?;
        let foreign_key_locks = locks::collect_foreign_key_locks_for_rows(
            state,
            schema,
            prepared.iter().filter_map(|row| row.updated.as_ref()),
            xid,
        )?;
        if foreign_key_locks
            .iter()
            .any(|lock| !state.row_locks.is_held(lock.key, xid, lock.mode))
        {
            context.request_row_lock_recheck_with_locks(foreign_key_locks.clone());
        }
        locks.extend(foreign_key_locks);
    }
    Ok(locks)
}

pub(in crate::executor) fn collect_delete_cte_locks(
    state: &DatabaseState,
    delete: &ast::Delete,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Vec<RequiredRowLock>> {
    let ast::FromTable::WithFromKeyword(from) = &delete.from else {
        return Ok(Vec::new());
    };
    let [target] = from.as_slice() else {
        return Ok(Vec::new());
    };
    let ast::TableFactor::Table {
        name: table_name,
        alias,
        args: None,
        ..
    } = &target.relation
    else {
        return Ok(Vec::new());
    };
    if !target.joins.is_empty() {
        return Ok(Vec::new());
    }
    let schema = state
        .catalog
        .require_named_table(&normalize_relation_name(table_name)?)?;
    let using = delete.using.as_deref().unwrap_or_default();
    let scope = create_mutation_scope(
        state,
        schema,
        alias.as_ref().map(|alias| &alias.name),
        using,
    )?;
    let occurrence = delete.span();
    let sql = delete.to_string();
    let targets = match context.get_prepared_mutation_targets(occurrence, snapshot.commit_seq) {
        Some(targets) => targets,
        None => {
            let source_rows = materialize_mutation_source_rows(
                state,
                using,
                &scope,
                schema.columns.len(),
                xid,
                snapshot,
                context,
            )?;
            let targets = collect_mutation_targets(
                state,
                schema,
                delete.selection.as_ref(),
                &scope,
                &source_rows,
                xid,
                snapshot,
                context,
                None,
            )?;
            validate_mutation_target_versions(state, schema, &targets, xid, snapshot)?;
            let targets = targets
                .into_iter()
                .map(
                    |(row_id, version_xmin, current, bound_row)| PreparedMutationTarget {
                        row_id,
                        version_xmin,
                        current,
                        bound_row,
                    },
                )
                .collect::<Vec<_>>();
            context.set_prepared_mutation_targets(
                occurrence,
                sql,
                snapshot.commit_seq,
                targets.clone(),
            );
            targets
        }
    };
    let locks = targets
        .iter()
        .map(|target| RequiredRowLock {
            key: RowLockKey {
                table_id: schema.id,
                row_id: target.row_id,
            },
            mode: RowLockMode::Update,
            mutation_candidate: Some(MutationCandidate {
                version_xmin: target.version_xmin,
                row: Some(target.current.clone()),
            }),
        })
        .collect::<Vec<_>>();
    if locks
        .iter()
        .any(|lock| !state.row_locks.is_held(lock.key, xid, lock.mode))
    {
        context.request_row_lock_recheck_with_locks(locks.clone());
    }
    Ok(locks)
}
