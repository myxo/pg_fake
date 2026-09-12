use std::collections::BTreeSet;

use sqlparser::ast;

use crate::{
    QueryResult, StatementResult,
    catalog::ConstraintId,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{
        DatabaseState, StatementExecutionContext, execute_statement, normalize_identifier,
        query::{describe_query_result_columns, execute_query, has_zero_limit},
        subqueries::materialize_uncorrelated_subqueries,
    },
    txn::{Snapshot, Xid},
};

use super::{
    MaterializedCte, convert_query_to_statement, execute_prepared_cte_query,
    is_data_modifying_query, materialize_query_ctes,
    recursive::{
        describe_recursive_cte_columns, execute_recursive_cte, materialize_recursive_query_ctes,
        resolve_direct_cte_demand, validate_recursive_cte,
    },
    references::{
        collect_cte_references, collect_reachable_cte_names, reject_cte_forward_references,
        replace_cte_references,
    },
};

pub(in crate::executor) fn prepare_cte_mutation_for_locking(
    state: &DatabaseState,
    query: &ast::Query,
    target_index: usize,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Option<ast::Statement>> {
    let with = query
        .with
        .as_ref()
        .expect("CTE lock preparation requires WITH");
    let names = with
        .cte_tables
        .iter()
        .map(|cte| normalize_identifier(&cte.alias.name))
        .collect::<Vec<_>>();
    let target = &with.cte_tables[target_index];
    if !is_data_modifying_query(&target.query) {
        return Ok(None);
    }
    let target_name = &names[target_index];
    let mut required = collect_cte_references(&target.query, &names);
    required.remove(target_name);
    let reachable = collect_reachable_cte_names(query);
    for (index, name) in names.iter().enumerate().take(target_index) {
        if !reachable.contains(name) || is_data_modifying_query(&with.cte_tables[index].query) {
            continue;
        }
        let mut dependencies = collect_cte_references(&with.cte_tables[index].query, &names);
        loop {
            let mut expanded = dependencies.clone();
            for (dependency_index, dependency) in names.iter().enumerate() {
                if dependencies.contains(dependency) {
                    expanded.extend(collect_cte_references(
                        &with.cte_tables[dependency_index].query,
                        &names,
                    ));
                }
            }
            if expanded == dependencies {
                break;
            }
            dependencies = expanded;
        }
        if !dependencies.contains(target_name) {
            required.insert(name.clone());
        }
    }
    loop {
        let mut expanded = required.clone();
        for (index, cte) in with.cte_tables.iter().enumerate() {
            if required.contains(&names[index]) {
                expanded.extend(collect_cte_references(&cte.query, &names));
            }
        }
        expanded.remove(target_name);
        if expanded == required {
            break;
        }
        required = expanded;
    }
    let mut materialized = Vec::new();
    while materialized.len() < required.len() {
        let mut progressed = false;
        for (index, cte) in with.cte_tables.iter().enumerate() {
            if !required.contains(&names[index])
                || materialized
                    .iter()
                    .any(|prepared: &MaterializedCte| prepared.name == names[index])
            {
                continue;
            }
            let dependencies = collect_cte_references(&cte.query, &names);
            if dependencies.iter().any(|dependency| {
                required.contains(dependency)
                    && !materialized
                        .iter()
                        .any(|prepared: &MaterializedCte| &prepared.name == dependency)
            }) {
                continue;
            }
            let mut cte_query = cte.query.as_ref().clone();
            replace_cte_references(&mut cte_query, &materialized);
            let occurrence = cte.alias.name.span;
            let mut result = match context.get_prepared_cte_result(occurrence, &names[index]) {
                Some(result) => result,
                None => {
                    let result = if is_data_modifying_query(&cte_query) {
                        let statement = convert_query_to_statement(cte_query.clone());
                        context.set_prepares_subquery_results(true);
                        let statement = materialize_uncorrelated_subqueries(
                            state, &statement, xid, snapshot, context,
                        )?;
                        context.set_prepares_subquery_results(false);
                        context.defer_cte_mutation(occurrence, names[index].clone(), statement);
                        return Ok(None);
                    } else {
                        let StatementResult::Query(result) =
                            execute_query(state, &cte_query, xid, snapshot, context)?
                        else {
                            unreachable!("read CTE returns query rows")
                        };
                        result
                    };
                    context.set_prepared_cte_result(
                        occurrence,
                        names[index].clone(),
                        result.clone(),
                    );
                    result
                }
            };
            if cte.alias.columns.len() > result.columns.len() {
                return Err(PgError::create(
                    SqlState::InvalidColumnReference,
                    "WITH query has fewer columns than specified in column list",
                ));
            }
            for (column, alias) in result.columns.iter_mut().zip(&cte.alias.columns) {
                column.name = normalize_identifier(&alias.name);
            }
            materialized.push(MaterializedCte {
                name: names[index].clone(),
                alias: cte.alias.clone(),
                result,
            });
            progressed = true;
        }
        if !progressed {
            return Ok(None);
        }
    }
    let mut target_query = target.query.as_ref().clone();
    replace_cte_references(&mut target_query, &materialized);
    let statement = convert_query_to_statement(target_query);
    context.set_prepares_subquery_results(true);
    let prepared = materialize_uncorrelated_subqueries(state, &statement, xid, snapshot, context);
    context.set_prepares_subquery_results(false);
    prepared.map(Some)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn materialize_statement_ctes(
    state: &mut DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementExecutionContext,
) -> Result<ast::Statement> {
    let ast::Statement::Query(query) = statement else {
        return Ok(statement.clone());
    };
    if query.with.is_none() {
        return Ok(convert_query_to_statement(materialize_query_ctes(
            state, query, xid, snapshot, context,
        )?));
    }
    let mut query = query.as_ref().clone();
    let with = query
        .with
        .take()
        .expect("WITH clause was checked as present");
    if with.recursive {
        if with
            .cte_tables
            .iter()
            .any(|cte| is_data_modifying_query(&cte.query))
        {
            return Ok(convert_query_to_statement(
                materialize_recursive_data_modifying_ctes(
                    state,
                    query,
                    with,
                    xid,
                    snapshot,
                    deferred_constraints,
                    defer_all,
                    context,
                )?,
            ));
        }
        return Ok(convert_query_to_statement(
            materialize_recursive_query_ctes(state, query, with, xid, snapshot, context)?,
        ));
    }
    let names = with
        .cte_tables
        .iter()
        .map(|cte| normalize_identifier(&cte.alias.name))
        .collect::<Vec<_>>();
    let mut body = query.clone();
    body.with = None;
    let mut consumed = collect_cte_references(&body, &names);
    for cte in &with.cte_tables {
        consumed.extend(collect_cte_references(&cte.query, &names));
    }
    let mut reachable = collect_cte_references(&body, &names);
    reachable.extend(
        with.cte_tables
            .iter()
            .filter(|cte| is_data_modifying_query(&cte.query))
            .map(|cte| normalize_identifier(&cte.alias.name)),
    );
    let mut mutation_required = with
        .cte_tables
        .iter()
        .filter(|cte| is_data_modifying_query(&cte.query))
        .map(|cte| normalize_identifier(&cte.alias.name))
        .collect::<BTreeSet<_>>();
    for (index, cte) in with.cte_tables.iter().enumerate().rev() {
        if reachable.contains(&names[index]) {
            reachable.extend(collect_cte_references(&cte.query, &names[..index]));
        }
        if mutation_required.contains(&names[index]) {
            mutation_required.extend(collect_cte_references(&cte.query, &names[..index]));
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
        let modifying = is_data_modifying_query(&cte.query);
        let mut cte_query = *cte.query;
        reject_cte_forward_references(&cte_query, &names[index..], &state.catalog)?;
        if !reachable.contains(&name) {
            continue;
        }
        replace_cte_references(&mut cte_query, &ctes);
        let result = if modifying {
            if let Some(result) = context.get_executed_cte_result(cte.alias.name.span, &name) {
                result
            } else {
                cte_query = materialize_query_ctes(state, &cte_query, xid, snapshot, context)?;
                let cte_statement = convert_query_to_statement(cte_query);
                let cte_statement = materialize_uncorrelated_subqueries(
                    state,
                    &cte_statement,
                    xid,
                    snapshot,
                    context,
                )?;
                match execute_statement(
                    state,
                    &cte_statement,
                    xid,
                    snapshot,
                    deferred_constraints,
                    defer_all,
                    context,
                    None,
                )? {
                    StatementResult::Query(result) => result,
                    StatementResult::Affected(_) if consumed.contains(&name) => {
                        return Err(PgError::create(
                            SqlState::FeatureNotSupported,
                            "WITH query does not have a RETURNING clause",
                        ));
                    }
                    StatementResult::Affected(_) => QueryResult {
                        columns: Vec::new(),
                        rows: Vec::new(),
                    },
                }
            }
        } else if skips_rows && !mutation_required.contains(&name) {
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
    Ok(convert_query_to_statement(materialize_query_ctes(
        state, &query, xid, snapshot, context,
    )?))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materialize_recursive_data_modifying_ctes(
    state: &mut DatabaseState,
    mut query: ast::Query,
    with: ast::With,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementExecutionContext,
) -> Result<ast::Query> {
    let names = with
        .cte_tables
        .iter()
        .map(|cte| normalize_identifier(&cte.alias.name))
        .collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    for name in &names {
        if !seen.insert(name.clone()) {
            return Err(PgError::create(
                SqlState::SyntaxError,
                format!("WITH query name {name:?} specified more than once"),
            ));
        }
    }
    let mut body = query.clone();
    body.with = None;
    let mut consumed = collect_cte_references(&body, &names);
    for cte in &with.cte_tables {
        consumed.extend(collect_cte_references(&cte.query, &names));
    }
    let mut reachable = collect_cte_references(&body, &names);
    reachable.extend(
        with.cte_tables
            .iter()
            .filter(|cte| is_data_modifying_query(&cte.query))
            .map(|cte| normalize_identifier(&cte.alias.name)),
    );
    loop {
        let mut expanded = reachable.clone();
        for (index, cte) in with.cte_tables.iter().enumerate() {
            if reachable.contains(&names[index]) {
                expanded.extend(collect_cte_references(&cte.query, &names));
            }
        }
        if expanded == reachable {
            break;
        }
        reachable = expanded;
    }
    let mut mutation_required = with
        .cte_tables
        .iter()
        .filter(|cte| is_data_modifying_query(&cte.query))
        .map(|cte| normalize_identifier(&cte.alias.name))
        .collect::<BTreeSet<_>>();
    loop {
        let mut expanded = mutation_required.clone();
        for (index, cte) in with.cte_tables.iter().enumerate() {
            if mutation_required.contains(&names[index]) {
                expanded.extend(collect_cte_references(&cte.query, &names));
            }
        }
        if expanded == mutation_required {
            break;
        }
        mutation_required = expanded;
    }
    let skips_rows = has_zero_limit(&query);
    let mut pending = with.cte_tables.into_iter().map(Some).collect::<Vec<_>>();
    let mut ctes = Vec::new();
    while ctes.len() < reachable.len() {
        let mut progressed = false;
        for index in 0..pending.len() {
            let Some(cte) = pending[index].as_ref() else {
                continue;
            };
            let name = &names[index];
            if !reachable.contains(name) {
                pending[index] = None;
                continue;
            }
            let dependencies = collect_cte_references(&cte.query, &names);
            if dependencies.iter().any(|dependency| {
                dependency != name
                    && reachable.contains(dependency)
                    && !ctes
                        .iter()
                        .any(|cte: &MaterializedCte| &cte.name == dependency)
            }) {
                continue;
            }
            let cte = pending[index]
                .take()
                .expect("pending CTE was checked as present");
            let modifying = is_data_modifying_query(&cte.query);
            if modifying && dependencies.contains(name) {
                return Err(PgError::create(
                    SqlState::InvalidRecursion,
                    "recursive query must not contain data-modifying statements",
                ));
            }
            let mut cte_query = *cte.query;
            replace_cte_references(&mut cte_query, &ctes);
            let recursive = !modifying && validate_recursive_cte(&cte_query, name)?;
            let demand = if recursive {
                resolve_direct_cte_demand(&mut query, name, context)?
            } else {
                None
            };
            let mut result = if modifying {
                if let Some(result) = context.get_executed_cte_result(cte.alias.name.span, name) {
                    result
                } else {
                    cte_query = materialize_query_ctes(state, &cte_query, xid, snapshot, context)?;
                    let cte_statement = convert_query_to_statement(cte_query);
                    let cte_statement = materialize_uncorrelated_subqueries(
                        state,
                        &cte_statement,
                        xid,
                        snapshot,
                        context,
                    )?;
                    match execute_statement(
                        state,
                        &cte_statement,
                        xid,
                        snapshot,
                        deferred_constraints,
                        defer_all,
                        context,
                        None,
                    )? {
                        StatementResult::Query(result) => result,
                        StatementResult::Affected(_) if consumed.contains(name) => {
                            return Err(PgError::create(
                                SqlState::FeatureNotSupported,
                                "WITH query does not have a RETURNING clause",
                            ));
                        }
                        StatementResult::Affected(_) => QueryResult {
                            columns: Vec::new(),
                            rows: Vec::new(),
                        },
                    }
                }
            } else if skips_rows && !mutation_required.contains(name) {
                QueryResult {
                    columns: if recursive {
                        describe_recursive_cte_columns(state, &cte_query, &cte.alias, name)?
                    } else {
                        describe_query_result_columns(
                            state,
                            &ast::Statement::Query(Box::new(cte_query.clone())),
                        )?
                    },
                    rows: Vec::new(),
                }
            } else if recursive {
                execute_recursive_cte(
                    state, &cte_query, &cte.alias, name, demand, xid, snapshot, context,
                )?
            } else {
                execute_prepared_cte_query(
                    state,
                    cte.alias.name.span,
                    name,
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
            for (column, alias) in result.columns.iter_mut().zip(&cte.alias.columns) {
                column.name = normalize_identifier(&alias.name);
            }
            ctes.push(MaterializedCte {
                name: name.clone(),
                alias: cte.alias,
                result,
            });
            progressed = true;
        }
        if !progressed {
            return reject_unsupported("mutual recursion between WITH items is not implemented");
        }
    }
    replace_cte_references(&mut query, &ctes);
    Ok(query)
}
