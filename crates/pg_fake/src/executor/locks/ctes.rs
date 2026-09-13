use super::{
    RequiredRowLock, collect_required_row_locks, insert::collect_triggered_insert_fallback_locks,
};
use crate::executor::{
    DatabaseState, StatementContext, ctes, normalize_identifier, query, resolve_insert_table_name,
    writes,
};
use crate::{
    error::Result,
    txn::{Snapshot, Xid},
};
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn collect_required_cte_row_locks(
    state: &DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Vec<RequiredRowLock>> {
    let ast::Statement::Query(query) = statement else {
        return Ok(Vec::new());
    };
    query::resolve_query_lock_targets(&state.catalog, query, None, &[], &[])?;
    if query::has_zero_limit(query)
        && query.with.as_ref().is_none_or(|with| {
            with.cte_tables.iter().all(|cte| {
                !matches!(
                    cte.query.body.as_ref(),
                    ast::SetExpr::Insert(_) | ast::SetExpr::Update(_) | ast::SetExpr::Delete(_)
                )
            })
        })
    {
        return Ok(Vec::new());
    }
    let reachable = ctes::collect_reachable_cte_names(query);
    let cte_names = query
        .with
        .as_ref()
        .map(|with| {
            with.cte_tables
                .iter()
                .map(|cte| normalize_identifier(&cte.alias.name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut locks = Vec::new();
    if let Some(with) = &query.with {
        for (cte_index, cte) in with.cte_tables.iter().enumerate() {
            let name = normalize_identifier(&cte.alias.name);
            if !reachable.contains(&name)
                || context
                    .get_executed_cte_result(cte.alias.name.span, &name)
                    .is_some()
            {
                continue;
            }
            if !matches!(
                cte.query.body.as_ref(),
                ast::SetExpr::Insert(_) | ast::SetExpr::Update(_) | ast::SetExpr::Delete(_)
            ) {
                continue;
            }
            let prepared = ctes::prepare_cte_mutation_for_locking(
                state, query, cte_index, xid, snapshot, context,
            )?;
            if context.requires_row_lock_recheck() {
                locks.extend(context.take_row_lock_recheck_locks());
                return Ok(locks);
            }
            if let Some(pending) = context.get_pending_cte_mutation() {
                locks.extend(collect_required_cte_row_locks(
                    state,
                    &pending.statement,
                    xid,
                    snapshot,
                    context,
                )?);
                match &pending.statement {
                    ast::Statement::Update(update) => locks.extend(
                        writes::collect_update_cte_locks(state, update, xid, snapshot, context)?,
                    ),
                    ast::Statement::Delete(delete) => locks.extend(
                        writes::collect_delete_cte_locks(state, delete, xid, snapshot, context)?,
                    ),
                    _ => locks.extend(collect_required_row_locks(
                        state,
                        &pending.statement,
                        xid,
                        snapshot,
                        context,
                    )?),
                }
                if context.requires_row_lock_recheck() {
                    locks.extend(context.take_row_lock_recheck_locks());
                }
                return Ok(locks);
            }
            let statement = prepared
                .unwrap_or_else(|| ctes::convert_query_to_statement(cte.query.as_ref().clone()));
            locks.extend(collect_required_cte_row_locks(
                state, &statement, xid, snapshot, context,
            )?);
            if context.requires_row_lock_recheck() {
                locks.extend(context.take_row_lock_recheck_locks());
                return Ok(locks);
            }
            match &statement {
                ast::Statement::Update(update) => {
                    locks.extend(writes::collect_update_cte_locks(
                        state, update, xid, snapshot, context,
                    )?);
                    if context.requires_row_lock_recheck() {
                        context.take_row_lock_recheck_locks();
                        return Ok(locks);
                    }
                    continue;
                }
                ast::Statement::Delete(delete) => {
                    locks.extend(writes::collect_delete_cte_locks(
                        state, delete, xid, snapshot, context,
                    )?);
                    if context.requires_row_lock_recheck() {
                        context.take_row_lock_recheck_locks();
                        return Ok(locks);
                    }
                    continue;
                }
                _ => {}
            }
            if let ast::Statement::Insert(insert) = &statement {
                let schema = state
                    .catalog
                    .require_named_table(&resolve_insert_table_name(&insert.table)?)?;
                let mut has_subquery = false;
                if let Some(source) = &insert.source {
                    let _ = ast::visit_expressions(source, |expression| {
                        if matches!(
                            expression,
                            ast::Expr::Subquery(_)
                                | ast::Expr::Exists { .. }
                                | ast::Expr::InSubquery { .. }
                        ) {
                            has_subquery = true;
                            return std::ops::ControlFlow::Break(());
                        }
                        std::ops::ControlFlow::Continue(())
                    });
                }
                let can_prepare = insert.source.as_ref().is_none_or(|source| {
                    !has_subquery
                        && (matches!(source.body.as_ref(), ast::SetExpr::Values(_))
                            || ctes::collect_cte_references(source, &cte_names).is_empty())
                });
                if !schema.triggers.is_empty() && !can_prepare {
                    locks.extend(collect_triggered_insert_fallback_locks(
                        state, insert, schema, xid, context,
                    )?);
                    continue;
                }
            }
            locks.extend(collect_required_row_locks(
                state, &statement, xid, snapshot, context,
            )?);
            if context.requires_row_lock_recheck() {
                locks.extend(context.take_row_lock_recheck_locks());
                return Ok(locks);
            }
        }
    }
    Ok(locks)
}
