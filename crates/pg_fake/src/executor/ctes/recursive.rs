use std::collections::BTreeSet;

use sqlparser::ast::{self, VisitMut as _};

use crate::{
    ColumnMeta, QueryResult, StatementResult,
    catalog::Catalog,
    coercion,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{
        DatabaseState, StatementExecutionContext,
        expressions::validate_equality_type,
        normalize_identifier, normalize_unqualified_object_name,
        query::{
            are_rows_not_distinct, contains_query_aggregate, describe_query_result_columns,
            execute_query, has_zero_limit, resolve_select_limit,
            set_operations::{
                coerce_set_rows, remove_set_duplicates, resolve_set_columns, sort_set_rows,
            },
        },
        scope::infer_query_output_columns,
    },
    txn::{Snapshot, Xid},
    value::{BaseType, PgType, Value},
};

use super::{
    MaterializedCte, execute_prepared_cte_query,
    references::{collect_cte_references, replace_cte_references},
};

struct RecursiveReferenceCounter<'a> {
    name: &'a str,
    masked: Vec<Vec<String>>,
    count: usize,
}

struct RecursivePlacementValidator<'a> {
    name: &'a str,
    invalid: bool,
}

impl ast::VisitorMut for RecursiveReferenceCounter<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.masked.push(
            query
                .with
                .as_ref()
                .map(|with| {
                    with.cte_tables
                        .iter()
                        .map(|cte| normalize_identifier(&cte.alias.name))
                        .collect()
                })
                .unwrap_or_default(),
        );
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.masked.pop().expect("visited query pushed CTE mask");
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        let ast::TableFactor::Table {
            name, args: None, ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(name) = normalize_unqualified_object_name(name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if name == self.name && !self.masked.iter().any(|masked| masked.contains(&name)) {
            self.count += 1;
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn count_recursive_references(expression: &ast::SetExpr, name: &str) -> usize {
    let mut query = create_set_expression_query(expression.clone());
    let mut counter = RecursiveReferenceCounter {
        name,
        masked: Vec::new(),
        count: 0,
    };
    let _ = query.visit(&mut counter);
    counter.count
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn count_query_recursive_references(query: &ast::Query, name: &str) -> usize {
    let mut query = query.clone();
    let mut counter = RecursiveReferenceCounter {
        name,
        masked: Vec::new(),
        count: 0,
    };
    let _ = query.visit(&mut counter);
    counter.count
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn count_factor_recursive_references(factor: &ast::TableFactor, name: &str) -> usize {
    let mut factor = factor.clone();
    let mut counter = RecursiveReferenceCounter {
        name,
        masked: Vec::new(),
        count: 0,
    };
    let _ = factor.visit(&mut counter);
    counter.count
}

impl ast::VisitorMut for RecursivePlacementValidator<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        let query = match expression {
            ast::Expr::Subquery(query)
            | ast::Expr::Exists {
                subquery: query, ..
            } => Some(query),
            ast::Expr::InSubquery { subquery, .. } => Some(subquery),
            _ => None,
        };
        if query.is_some_and(|query| count_query_recursive_references(query, self.name) != 0) {
            self.invalid = true;
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        if let ast::TableFactor::Derived { subquery, .. } = factor
            && count_query_recursive_references(subquery, self.name) != 0
        {
            self.invalid = true;
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_select(&mut self, select: &mut ast::Select) -> std::ops::ControlFlow<Self::Break> {
        for table in &select.from {
            let mut left_references = count_factor_recursive_references(&table.relation, self.name);
            for join in &table.joins {
                let right_references = count_factor_recursive_references(&join.relation, self.name);
                let invalid = match &join.join_operator {
                    ast::JoinOperator::Left(_) | ast::JoinOperator::LeftOuter(_) => {
                        right_references != 0
                    }
                    ast::JoinOperator::Right(_) | ast::JoinOperator::RightOuter(_) => {
                        left_references != 0
                    }
                    ast::JoinOperator::FullOuter(_) => {
                        left_references != 0 || right_references != 0
                    }
                    _ => false,
                };
                if invalid {
                    self.invalid = true;
                    return std::ops::ControlFlow::Break(());
                }
                left_references += right_references;
            }
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validate_recursive_placements(expression: &ast::SetExpr, name: &str) -> Result<()> {
    let mut query = create_set_expression_query(expression.clone());
    let mut validator = RecursivePlacementValidator {
        name,
        invalid: false,
    };
    let _ = query.visit(&mut validator);
    if validator.invalid {
        Err(PgError::create(
            SqlState::InvalidRecursion,
            format!(
                "recursive reference to query {name:?} must not appear within a subquery or outer join"
            ),
        ))
    } else {
        Ok(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn validate_recursive_cte(query: &ast::Query, name: &str) -> Result<bool> {
    let references = count_recursive_references(&query.body, name);
    if references == 0 {
        return Ok(false);
    }
    let ast::SetExpr::SetOperation {
        left,
        op: ast::SetOperator::Union,
        set_quantifier:
            ast::SetQuantifier::All | ast::SetQuantifier::None | ast::SetQuantifier::Distinct,
        right,
    } = query.body.as_ref()
    else {
        return Err(PgError::create(
            SqlState::InvalidRecursion,
            format!(
                "recursive query {name:?} does not have the form non-recursive-term UNION [ALL] recursive-term"
            ),
        ));
    };
    if count_recursive_references(left, name) != 0 {
        return Err(PgError::create(
            SqlState::InvalidRecursion,
            format!(
                "recursive reference to query {name:?} must not appear within its non-recursive term"
            ),
        ));
    }
    if count_recursive_references(right, name) != 1 {
        return Err(PgError::create(
            SqlState::InvalidRecursion,
            format!("recursive reference to query {name:?} must not appear more than once"),
        ));
    }
    validate_recursive_placements(right, name)?;
    if contains_query_aggregate(&create_set_expression_query((**right).clone())) {
        return Err(PgError::create(
            SqlState::InvalidRecursion,
            "aggregate functions are not allowed in a recursive query's recursive term",
        ));
    }
    Ok(true)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn create_set_expression_query(expression: ast::SetExpr) -> ast::Query {
    ast::Query {
        with: None,
        body: Box::new(expression),
        order_by: None,
        limit_clause: None,
        fetch: None,
        locks: Vec::new(),
        for_clause: None,
        settings: None,
        format_clause: None,
        pipe_operators: Vec::new(),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn validate_recursive_cte_types(catalog: &Catalog, query: &ast::Query) -> Result<()> {
    if crate::analyzer::count_parameters(&ast::Statement::Query(Box::new(query.clone())))? != 0 {
        return Ok(());
    }
    let ast::SetExpr::SetOperation { left, right, .. } = query.body.as_ref() else {
        unreachable!("recursive CTE shape was validated");
    };
    let seed = infer_query_output_columns(catalog, &create_set_expression_query((**left).clone()))?;
    let recursive =
        infer_query_output_columns(catalog, &create_set_expression_query((**right).clone()))?;
    if seed.len() != recursive.len() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "each set-operation query must have the same number of columns",
        ));
    }
    for ((_, seed), (_, recursive)) in seed.iter().zip(&recursive) {
        let Some(common) = coercion::resolve_common_type(seed.base, recursive.base) else {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "recursive query column types cannot be matched",
            ));
        };
        if common != seed.base || seed.base == recursive.base && seed.typmod != recursive.typmod {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "recursive query column type does not match non-recursive term",
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn materialize_recursive_query_ctes(
    state: &DatabaseState,
    mut query: ast::Query,
    with: ast::With,
    xid: Xid,
    snapshot: &Snapshot,
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
    let mut reachable = collect_cte_references(&body, &names);
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
            let mut cte_query = *cte.query;
            replace_cte_references(&mut cte_query, &ctes);
            let recursive = validate_recursive_cte(&cte_query, name)?;
            let demand = if recursive {
                resolve_direct_cte_demand(&mut query, name, context)?
            } else {
                None
            };
            let mut result = if skips_rows {
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn resolve_direct_cte_demand(
    query: &mut ast::Query,
    name: &str,
    context: &StatementExecutionContext,
) -> Result<Option<usize>> {
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        return Ok(None);
    };
    let ast::GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
        return Ok(None);
    };
    let [table] = select.from.as_slice() else {
        return Ok(None);
    };
    let ast::TableFactor::Table {
        name: table_name,
        args: None,
        ..
    } = &table.relation
    else {
        return Ok(None);
    };
    if query.order_by.is_some()
        || select.distinct.is_some()
        || select.selection.is_some()
        || select.having.is_some()
        || !group_by.is_empty()
        || !modifiers.is_empty()
        || !table.joins.is_empty()
        || normalize_unqualified_object_name(table_name)? != name
        || count_query_recursive_references(query, name) != 1
        || contains_query_aggregate(query)
    {
        return Ok(None);
    }
    let (limit, offset) = resolve_select_limit(query, context)?;
    let Some(limit) = limit else {
        return Ok(None);
    };
    let Some(ast::LimitClause::LimitOffset {
        limit: limit_expression,
        offset: offset_expression,
        ..
    }) = &mut query.limit_clause
    else {
        unreachable!("select limit was resolved");
    };
    if limit_expression.is_some() {
        *limit_expression = Some(crate::analyzer::create_typed_literal(
            Value::Int8(i64::try_from(limit).expect("LIMIT value originated as int8")),
            PgType::create(BaseType::Int8),
        ));
    }
    if let Some(offset_expression) = offset_expression {
        offset_expression.value = crate::analyzer::create_typed_literal(
            Value::Int8(i64::try_from(offset).expect("OFFSET value originated as int8")),
            PgType::create(BaseType::Int8),
        );
    }
    Ok(Some(offset.saturating_add(limit)))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_recursive_columns(
    seed: &[ColumnMeta],
    recursive: &[ColumnMeta],
) -> Result<Vec<ColumnMeta>> {
    let columns = resolve_set_columns(seed, recursive)?;
    for ((seed, recursive), column) in seed.iter().zip(recursive).zip(&columns) {
        if seed.type_oid != column.type_oid
            || seed.type_oid == recursive.type_oid && seed.typmod != recursive.typmod
        {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "recursive query column type does not match non-recursive term",
            ));
        }
    }
    Ok(seed.to_vec())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn describe_recursive_cte_columns(
    state: &DatabaseState,
    query: &ast::Query,
    alias: &ast::TableAlias,
    name: &str,
) -> Result<Vec<ColumnMeta>> {
    let ast::SetExpr::SetOperation { left, right, .. } = query.body.as_ref() else {
        unreachable!("recursive CTE shape was validated");
    };
    let mut seed_columns = describe_query_result_columns(
        state,
        &ast::Statement::Query(Box::new(create_set_expression_query((**left).clone()))),
    )?;
    if alias.columns.len() > seed_columns.len() {
        return Err(PgError::create(
            SqlState::InvalidColumnReference,
            "WITH query has fewer columns than specified in column list",
        ));
    }
    for (column, alias) in seed_columns.iter_mut().zip(&alias.columns) {
        column.name = normalize_identifier(&alias.name);
    }
    let mut recursive_query = create_set_expression_query((**right).clone());
    replace_cte_references(
        &mut recursive_query,
        &[MaterializedCte {
            name: name.to_owned(),
            alias: alias.clone(),
            result: QueryResult {
                columns: seed_columns.clone(),
                rows: Vec::new(),
            },
        }],
    );
    let recursive_columns =
        describe_query_result_columns(state, &ast::Statement::Query(Box::new(recursive_query)))?;
    resolve_recursive_columns(&seed_columns, &recursive_columns)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_recursive_cte(
    state: &DatabaseState,
    query: &ast::Query,
    alias: &ast::TableAlias,
    name: &str,
    output_demand: Option<usize>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<QueryResult> {
    let ast::SetExpr::SetOperation {
        left,
        set_quantifier,
        right,
        ..
    } = query.body.as_ref()
    else {
        unreachable!("recursive CTE shape was validated");
    };
    if query.fetch.is_some() || !query.locks.is_empty() || query.for_clause.is_some() {
        return reject_unsupported("recursive query clause is not implemented");
    }
    let columns = describe_recursive_cte_columns(state, query, alias, name)?;
    let distinct = matches!(
        set_quantifier,
        ast::SetQuantifier::None | ast::SetQuantifier::Distinct
    );
    if distinct {
        for column in &columns {
            validate_equality_type(
                BaseType::resolve_oid(column.type_oid)
                    .expect("recursive columns use supported PostgreSQL types"),
            )?;
        }
    }
    let (limit, offset) = resolve_select_limit(query, context)?;
    let generation_demand = query
        .order_by
        .is_none()
        .then(|| match (limit, output_demand) {
            (Some(limit), Some(output_demand)) => offset.saturating_add(limit.min(output_demand)),
            (Some(limit), None) => offset.saturating_add(limit),
            (None, Some(output_demand)) => offset.saturating_add(output_demand),
            (None, None) => usize::MAX,
        });
    let StatementResult::Query(mut seed) = execute_query(
        state,
        &create_set_expression_query((**left).clone()),
        xid,
        snapshot,
        context,
    )?
    else {
        unreachable!("recursive seed produces query rows");
    };
    if alias.columns.len() > seed.columns.len() {
        return Err(PgError::create(
            SqlState::InvalidColumnReference,
            "WITH query has fewer columns than specified in column list",
        ));
    }
    for (column, alias) in seed.columns.iter_mut().zip(&alias.columns) {
        column.name = normalize_identifier(&alias.name);
    }
    let mut rows = coerce_set_rows(seed.rows, &seed.columns, &columns)?;
    if distinct {
        rows = remove_set_duplicates(rows)?;
    }
    if let Some(generation_demand) = generation_demand {
        rows.truncate(generation_demand);
    }
    let mut working = rows.clone();
    while !working.is_empty()
        && generation_demand.is_none_or(|generation_demand| rows.len() < generation_demand)
    {
        let mut recursive_query = create_set_expression_query((**right).clone());
        replace_cte_references(
            &mut recursive_query,
            &[MaterializedCte {
                name: name.to_owned(),
                alias: alias.clone(),
                result: QueryResult {
                    columns: columns.clone(),
                    rows: working,
                },
            }],
        );
        let StatementResult::Query(result) =
            execute_query(state, &recursive_query, xid, snapshot, context)?
        else {
            unreachable!("recursive term produces query rows");
        };
        working = coerce_set_rows(result.rows, &result.columns, &columns)?;
        if distinct {
            working = remove_set_duplicates(working)?;
            let mut new_rows = Vec::new();
            for candidate in working {
                let mut duplicate = false;
                for existing in &rows {
                    if are_rows_not_distinct(existing, &candidate)? {
                        duplicate = true;
                        break;
                    }
                }
                if duplicate {
                    continue;
                }
                new_rows.push(candidate);
            }
            working = new_rows;
        }
        if let Some(generation_demand) = generation_demand {
            working.truncate(generation_demand - rows.len());
        }
        rows.extend(working.iter().cloned());
    }
    let mut result = QueryResult { columns, rows };
    sort_set_rows(&mut result.rows, &result.columns, query)?;
    result.rows = result
        .rows
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    Ok(result)
}
