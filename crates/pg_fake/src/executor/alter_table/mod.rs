use crate::executor::{
    DatabaseState, StatementContext,
    foreign_keys::{validate_foreign_key_definitions, validate_row_foreign_keys},
    normalize_relation_name,
    row_constraints::{validate_check_constraints, validate_not_null},
};
use crate::{
    StatementResult,
    catalog::ConstraintId,
    error::{PgError, Result, SqlState, reject_unsupported},
    storage::RowId,
    txn::{Snapshot, Xid},
    value::Value,
};
use sqlparser::ast;
use std::{collections::BTreeSet, sync::Arc};

mod columns;
mod constraints;
mod dependencies;
mod operations;

use operations::apply_table_operation;

struct RewrittenRow {
    row_id: RowId,
    version_xmin: Xid,
    original: Vec<Value>,
    row: Vec<Value>,
}

pub(super) fn execute_alter_table(
    state: &mut DatabaseState,
    alter: &ast::AlterTable,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementContext,
) -> Result<StatementResult> {
    let existing_sequences = state
        .sequence_values
        .lock()
        .expect("sequence storage is poisoned")
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    let result = apply_table_changes(
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

fn apply_table_changes(
    state: &mut DatabaseState,
    alter: &ast::AlterTable,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementContext,
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
        .map(|(row_id, version)| RewrittenRow {
            row_id,
            version_xmin: version.xmin,
            original: version.row.clone(),
            row: version.row,
        })
        .collect::<Vec<_>>();

    for operation in &alter.operations {
        apply_table_operation(state, &mut schema, &mut rows, operation, context)?;
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
        .replace_schema(Arc::new(schema.clone()));

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
