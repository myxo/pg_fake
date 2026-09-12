use super::{
    require_mutation_table,
    returning::{build_returning_plan, create_write_result, evaluate_returning_row},
    targets::{
        collect_mutation_targets, create_mutation_scope, has_mutated_target_in_command,
        materialize_mutation_source_rows,
    },
};
use crate::executor::{
    DatabaseState, RequiredRowLock, StatementContext, expressions::is_null_literal,
    foreign_keys::apply_referencing_foreign_key_actions, normalize_relation_name, query,
};
use crate::{
    StatementResult,
    catalog::ConstraintId,
    error::{PgError, Result, SqlState, reject_unsupported},
    txn::{Snapshot, Xid},
    value::BaseType,
};
use sqlparser::ast::{self, Spanned as _};
use std::collections::BTreeSet;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn execute_delete(
    state: &mut DatabaseState,
    delete: &ast::Delete,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementContext,
    mut mutation_targets: Option<Vec<RequiredRowLock>>,
) -> Result<StatementResult> {
    if !delete.tables.is_empty() || !delete.order_by.is_empty() || delete.limit.is_some() {
        return reject_unsupported("DELETE feature is not implemented");
    }
    let ast::FromTable::WithFromKeyword(from) = &delete.from else {
        return reject_unsupported("DELETE without FROM is not implemented");
    };
    if from.len() != 1 || !from[0].joins.is_empty() {
        return reject_unsupported("DELETE joins are not implemented");
    }
    let ast::TableFactor::Table {
        name: table_name,
        alias,
        args,
        ..
    } = &from[0].relation
    else {
        return reject_unsupported("DELETE target is not implemented");
    };
    if args.is_some() {
        return reject_unsupported("DELETE table functions are not implemented");
    }
    let schema = require_mutation_table(state, &normalize_relation_name(table_name)?)?;
    let using = delete.using.as_deref().unwrap_or_default();
    let scope = create_mutation_scope(
        state,
        &schema,
        alias.as_ref().map(|alias| &alias.name),
        using,
    )?;
    let returning = build_returning_plan(
        state,
        scope.clone(),
        schema.columns.len(),
        delete.returning.as_deref(),
    )?;
    if let Some(selection) = &delete.selection {
        let base = query::infer_query_expression_type(state, selection, &scope)?.base;
        if base != BaseType::Bool && !is_null_literal(selection) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let has_referencing_foreign_keys = state.catalog.has_referencing_foreign_keys(schema.id);
    let prepared_targets =
        context.take_prepared_mutation_targets(delete.span(), snapshot.commit_seq);
    if using.is_empty()
        && returning.is_none()
        && !has_referencing_foreign_keys
        && let Some(mutation_targets) = mutation_targets.take()
    {
        let targets = mutation_targets
            .into_iter()
            .filter(|required| required.key.table_id == schema.id)
            .collect::<Vec<_>>();
        let affected = targets.len() as u64;
        for required in targets {
            let candidate = required
                .mutation_candidate
                .expect("mutation target locks retain their selected version");
            state
                .tables
                .get_mut(&schema.id)
                .expect("catalog table must have storage")
                .mark_version_deleted(
                    required.key.row_id,
                    candidate.version_xmin,
                    xid,
                    context.command_id,
                );
        }
        if affected != 0 {
            state.mark_table_touched(xid, schema.id);
        }
        return Ok(StatementResult::Affected(affected));
    }
    let source_rows = materialize_mutation_source_rows(
        state,
        using,
        &scope,
        schema.columns.len(),
        xid,
        snapshot,
        context,
    )?;
    let targets = match prepared_targets {
        Some(targets) => targets
            .into_iter()
            .filter(|target| {
                !has_mutated_target_in_command(
                    state,
                    schema.id,
                    target.row_id,
                    target.version_xmin,
                    xid,
                    context.command_id,
                )
            })
            .map(|target| {
                (
                    target.row_id,
                    target.version_xmin,
                    target.current,
                    target.bound_row,
                )
            })
            .collect(),
        None => collect_mutation_targets(
            state,
            &schema,
            delete.selection.as_ref(),
            &scope,
            &source_rows,
            xid,
            snapshot,
            context,
            mutation_targets,
        )?,
    };
    let affected = targets.len() as u64;
    let mut returned_rows = Vec::new();
    for (row_id, version_xmin, row, bound_row) in targets {
        if has_referencing_foreign_keys {
            apply_referencing_foreign_key_actions(
                state,
                &schema,
                &row,
                None,
                xid,
                snapshot,
                deferred_constraints,
                defer_all,
                &mut BTreeSet::new(),
                context,
            )?;
        }
        state
            .tables
            .get_mut(&schema.id)
            .expect("catalog table must have storage")
            .mark_version_deleted(row_id, version_xmin, xid, context.command_id);
        state.mark_table_touched(xid, schema.id);
        evaluate_returning_row(
            state,
            returning.as_ref(),
            bound_row.as_deref().unwrap_or(&row),
            &mut returned_rows,
            xid,
            snapshot,
            context,
        )?;
    }
    Ok(create_write_result(affected, returning, returned_rows))
}
