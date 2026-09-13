use std::collections::BTreeSet;

use sqlparser::{
    ast::{self, VisitMut as _},
    tokenizer::Span,
};

use crate::{
    QueryResult,
    error::{PgError, Result, SqlState},
    executor::{
        DatabaseState, StatementContext, normalize_identifier,
        query::{describe_query_result_columns, execute_query, has_zero_limit},
    },
    txn::{Snapshot, Xid},
};

mod analysis;
mod mutations;
mod recursive;
mod references;
pub(super) mod scope;

pub(crate) use analysis::expand_ctes_for_analysis;
pub(super) use analysis::inline_query_ctes;
pub(super) use analysis::{InlineCte, collect_query_cte_scope, inline_query_with_cte_scope};
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
    source: CteSource,
}

enum CteSource {
    Rows(QueryResult),
    Query {
        query: Box<ast::Query>,
        columns: Vec<crate::ColumnMeta>,
    },
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
    context: &StatementContext,
) -> Result<QueryResult> {
    let initplan = super::lateral::InitplanKey::Cte(occurrence, name.to_owned());
    if let Some(result) = context
        .lateral_initplans
        .lock()
        .expect("lateral initplans mutex is poisoned")
        .get_result(&initplan)
    {
        return Ok(result);
    }
    if let Some(result) = context.get_prepared_cte_result(occurrence, name) {
        return Ok(result);
    }
    let result = execute_query(state, query, xid, snapshot, context)?.result;
    context
        .lateral_initplans
        .lock()
        .expect("lateral initplans mutex is poisoned")
        .set_result(&initplan, result.clone());
    Ok(result)
}

struct DerivedCteMaterializer<'a> {
    state: &'a DatabaseState,
    xid: Xid,
    snapshot: &'a Snapshot,
    context: &'a StatementContext,
    error: Option<PgError>,
    lateral_depth: usize,
}

impl ast::VisitorMut for DerivedCteMaterializer<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        if matches!(factor, ast::TableFactor::Derived { lateral: true, .. }) {
            self.lateral_depth += 1;
        }
        if self.lateral_depth > 0 {
            return std::ops::ControlFlow::Continue(());
        }
        let ast::TableFactor::Derived { subquery, .. } = factor else {
            return std::ops::ControlFlow::Continue(());
        };
        if subquery.with.is_none() {
            return std::ops::ControlFlow::Continue(());
        }
        match materialize_query_ctes(self.state, subquery, self.xid, self.snapshot, self.context) {
            Ok(materialized) => **subquery = materialized,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<()> {
        if matches!(factor, ast::TableFactor::Derived { lateral: true, .. }) {
            self.lateral_depth -= 1;
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
    context: &StatementContext,
) -> Result<ast::Query> {
    let mut query = query.clone();
    let Some(with) = query.with.take() else {
        let mut materializer = DerivedCteMaterializer {
            lateral_depth: 0,
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
        replace_cte_references(&mut cte_query, &ctes, Some(context));
        if context.lateral_invocation || super::query::contains_row_locks(&cte_query) {
            let analysis = inline_query_ctes(&cte_query, &state.catalog, Some(state), true)?;
            let mut columns =
                describe_query_result_columns(state, &ast::Statement::Query(Box::new(analysis)))?;
            if cte.alias.columns.len() > columns.len() {
                return Err(PgError::create(
                    SqlState::InvalidColumnReference,
                    "WITH query has fewer columns than specified in column list",
                ));
            }
            for (column, alias) in columns.iter_mut().zip(&cte.alias.columns) {
                column.name = normalize_identifier(&alias.name);
            }
            ctes.push(MaterializedCte {
                name,
                alias: cte.alias,
                source: CteSource::Query {
                    query: Box::new(cte_query),
                    columns,
                },
            });
            continue;
        }
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
            source: CteSource::Rows(result),
        });
    }
    replace_cte_references(&mut query, &ctes, Some(context));
    Ok(query)
}
