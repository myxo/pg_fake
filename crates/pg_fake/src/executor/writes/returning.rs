use crate::executor::{DatabaseState, StatementContext, query, scope::BoundScope};
use crate::{
    ColumnMeta, QueryResult, StatementResult,
    error::Result,
    txn::{Snapshot, Xid},
    value::Value,
};
use sqlparser::ast;

pub(super) struct ReturningPlan<'a> {
    scope: BoundScope,
    projections: Vec<query::ProjectionSource<'a>>,
    columns: Vec<ColumnMeta>,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn build_returning_plan<'a>(
    state: &DatabaseState,
    scope: BoundScope,
    target_columns: usize,
    returning: Option<&'a [ast::SelectItem]>,
) -> Result<Option<ReturningPlan<'a>>> {
    let Some(returning) = returning else {
        return Ok(None);
    };
    let (projections, columns) =
        query::build_mutation_projection_plan(state, returning, &scope, target_columns)?;
    Ok(Some(ReturningPlan {
        scope,
        projections,
        columns,
    }))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_returning_row(
    state: &DatabaseState,
    returning: Option<&ReturningPlan<'_>>,
    row: &[Value],
    rows: &mut Vec<Vec<Value>>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<()> {
    let Some(returning) = returning else {
        return Ok(());
    };
    rows.push(query::evaluate_projection_values(
        state,
        &returning.projections,
        &returning.scope,
        row,
        None,
        xid,
        snapshot,
        context,
    )?);
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn create_write_result(
    affected: u64,
    returning: Option<ReturningPlan<'_>>,
    rows: Vec<Vec<Value>>,
) -> StatementResult {
    match returning {
        Some(returning) => StatementResult::Query(QueryResult {
            columns: returning.columns,
            rows,
        }),
        None => StatementResult::Affected(affected),
    }
}
