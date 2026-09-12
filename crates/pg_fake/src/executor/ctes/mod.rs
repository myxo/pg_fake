use std::collections::BTreeSet;

use sqlparser::{
    ast::{self, VisitMut as _},
    tokenizer::Span,
};

use crate::{
    QueryResult, StatementResult,
    error::{PgError, Result, SqlState},
    executor::{
        DatabaseState, StatementExecutionContext, normalize_identifier,
        query::{describe_query_result_columns, execute_query, has_zero_limit},
    },
    txn::{Snapshot, Xid},
};

mod analysis;
mod mutations;
mod recursive;
mod references;

pub(crate) use analysis::expand_ctes_for_analysis;
pub(crate) use mutations::materialize_statement_ctes;
pub(super) use mutations::prepare_cte_mutation_for_locking;
pub(super) use references::{
    collect_cte_references, collect_reachable_cte_names, contains_query_ctes,
};

use recursive::materialize_recursive_query_ctes;
use references::{reject_cte_forward_references, replace_cte_references};

struct MaterializedCte {
    name: String,
    alias: ast::TableAlias,
    result: QueryResult,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn convert_query_to_statement(query: ast::Query) -> ast::Statement {
    match query.body.as_ref() {
        ast::SetExpr::Insert(statement)
        | ast::SetExpr::Update(statement)
        | ast::SetExpr::Delete(statement) => statement.clone(),
        _ => ast::Statement::Query(Box::new(query)),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_data_modifying_query(query: &ast::Query) -> bool {
    matches!(
        query.body.as_ref(),
        ast::SetExpr::Insert(_) | ast::SetExpr::Update(_) | ast::SetExpr::Delete(_)
    )
}

fn execute_prepared_cte_query(
    state: &DatabaseState,
    occurrence: Span,
    name: &str,
    query: &ast::Query,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<QueryResult> {
    if let Some(result) = context.get_prepared_cte_result(occurrence, name) {
        return Ok(result);
    }
    let StatementResult::Query(result) = execute_query(state, query, xid, snapshot, context)?
    else {
        unreachable!("CTE query produces query rows")
    };
    Ok(result)
}

struct DerivedCteMaterializer<'a> {
    state: &'a DatabaseState,
    xid: Xid,
    snapshot: &'a Snapshot,
    context: &'a StatementExecutionContext,
    error: Option<PgError>,
}

impl ast::VisitorMut for DerivedCteMaterializer<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        let ast::TableFactor::Derived { subquery, .. } = factor else {
            return std::ops::ControlFlow::Continue(());
        };
        if subquery.with.is_none() {
            return std::ops::ControlFlow::Continue(());
        }
        match materialize_query_ctes(self.state, subquery, self.xid, self.snapshot, self.context) {
            Ok(materialized) => *subquery = Box::new(materialized),
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn materialize_query_ctes(
    state: &DatabaseState,
    query: &ast::Query,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<ast::Query> {
    let mut query = query.clone();
    let Some(with) = query.with.take() else {
        let mut materializer = DerivedCteMaterializer {
            state,
            xid,
            snapshot,
            context,
            error: None,
        };
        let _ = query.visit(&mut materializer);
        if let Some(error) = materializer.error {
            return Err(error);
        }
        return Ok(query);
    };
    if with
        .cte_tables
        .iter()
        .any(|cte| is_data_modifying_query(&cte.query))
    {
        return Err(PgError::create(
            SqlState::FeatureNotSupported,
            "WITH clause containing a data-modifying statement must be at the top level",
        ));
    }
    if with.recursive {
        return materialize_recursive_query_ctes(state, query, with, xid, snapshot, context);
    }
    let names = with
        .cte_tables
        .iter()
        .map(|cte| normalize_identifier(&cte.alias.name))
        .collect::<Vec<_>>();
    let mut body = query.clone();
    body.with = None;
    let mut reachable = collect_cte_references(&body, &names);
    for (index, cte) in with.cte_tables.iter().enumerate().rev() {
        if reachable.contains(&names[index]) {
            reachable.extend(collect_cte_references(&cte.query, &names[..index]));
        }
    }
    let skips_rows = has_zero_limit(&query);
    let mut ctes = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, cte) in with.cte_tables.into_iter().enumerate() {
        let name = normalize_identifier(&cte.alias.name);
        if !seen.insert(name.clone()) {
            return Err(PgError::create(
                SqlState::SyntaxError,
                format!("WITH query name {name:?} specified more than once"),
            ));
        }
        let mut cte_query = *cte.query;
        reject_cte_forward_references(&cte_query, &names[index..], &state.catalog)?;
        if !reachable.contains(&name) {
            continue;
        }
        replace_cte_references(&mut cte_query, &ctes);
        let result = if skips_rows {
            QueryResult {
                columns: describe_query_result_columns(
                    state,
                    &ast::Statement::Query(Box::new(cte_query.clone())),
                )?,
                rows: Vec::new(),
            }
        } else {
            execute_prepared_cte_query(
                state,
                cte.alias.name.span,
                &name,
                &cte_query,
                xid,
                snapshot,
                context,
            )?
        };
        if cte.alias.columns.len() > result.columns.len() {
            return Err(PgError::create(
                SqlState::InvalidColumnReference,
                "WITH query has fewer columns than specified in column list",
            ));
        }
        let mut result = result;
        for (column, alias) in result.columns.iter_mut().zip(&cte.alias.columns) {
            column.name = normalize_identifier(&alias.name);
        }
        ctes.push(MaterializedCte {
            name,
            alias: cte.alias,
            result,
        });
    }
    replace_cte_references(&mut query, &ctes);
    Ok(query)
}
