use std::collections::BTreeSet;

use sqlparser::ast;

use crate::{
    StatementResult,
    error::{PgError, Result, SqlState, reject_unsupported},
    txn::{Snapshot, Xid},
};

use super::{DatabaseState, StatementContext, normalize_relation_name};

pub(super) fn execute_truncate(
    state: &mut DatabaseState,
    truncate: &ast::Truncate,
    xid: Xid,
    _snapshot: &Snapshot,
    _context: &StatementContext,
) -> Result<StatementResult> {
    if truncate.if_exists || truncate.partitions.is_some() || truncate.on_cluster.is_some() {
        return reject_unsupported("TRUNCATE option is not implemented");
    }
    if truncate
        .table_names
        .iter()
        .any(|target| target.only || target.has_asterisk)
    {
        return reject_unsupported("TRUNCATE inheritance targets are not implemented");
    }
    let mut targets = truncate
        .table_names
        .iter()
        .map(|target| {
            let name = normalize_relation_name(&target.name)?;
            state
                .catalog
                .require_named_table(&name)
                .map(|table| table.id)
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let cascade = matches!(truncate.cascade, Some(ast::CascadeOption::Cascade));
    let mut pending = targets.iter().copied().collect::<Vec<_>>();
    while let Some(parent) = pending.pop() {
        for (referencing, foreign_key) in state.catalog.collect_referencing_foreign_keys(parent) {
            if targets.contains(&referencing.id) {
                continue;
            }
            if !cascade {
                let parent = state.catalog.require_table_by_id(parent)?;
                return Err(PgError::create(
                    SqlState::FeatureNotSupported,
                    format!(
                        "cannot truncate table {:?} because constraint {:?} on table {:?} references it",
                        parent.name, foreign_key.name, referencing.name
                    ),
                ));
            }
            targets.insert(referencing.id);
            pending.push(referencing.id);
        }
    }
    let restart_identity = matches!(
        truncate.identity,
        Some(ast::TruncateIdentityOption::Restart)
    );
    let sequences = if restart_identity {
        state
            .catalog
            .iterate_sequences()
            .filter(|sequence| {
                sequence
                    .owned_by
                    .as_ref()
                    .is_some_and(|(owner, _)| targets.contains(owner))
            })
            .cloned()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    for table_id in targets {
        state
            .tables
            .get_mut(&table_id)
            .expect("catalog table must have storage")
            .truncate_all(xid);
        state.mark_table_touched(xid, table_id);
    }
    for sequence in &sequences {
        state.reset_sequence_transactionally(xid, sequence);
    }
    Ok(StatementResult::Affected(0))
}
