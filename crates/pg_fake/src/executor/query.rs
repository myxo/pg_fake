use super::*;
use ast::VisitMut as _;
use sqlparser::ast;

struct MaterializedCte {
    name: String,
    alias: ast::TableAlias,
    result: QueryResult,
}

#[derive(Clone)]
struct InlineCte {
    name: String,
    query: Box<ast::Query>,
    alias: ast::TableAlias,
    masked_names: Vec<String>,
}

struct InlineCteReferenceReplacer<'a> {
    state: &'a DatabaseState,
    ctes: &'a [InlineCte],
    masked: Vec<Vec<String>>,
    pending_mask: Option<Vec<String>>,
    error: Option<PgError>,
}

struct CteForwardReferenceDetector<'a> {
    catalog: &'a Catalog,
    names: &'a [String],
    error: Option<PgError>,
}

struct RecursiveReferenceCounter<'a> {
    name: &'a str,
    masked: Vec<Vec<String>>,
    count: usize,
}

struct RecursivePlacementValidator<'a> {
    name: &'a str,
    invalid: bool,
}

struct QueryAggregateDetector {
    query_depth: usize,
    found: bool,
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

impl ast::VisitorMut for QueryAggregateDetector {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth -= 1;
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if self.query_depth == 1
            && matches!(expression, ast::Expr::Function(function) if is_aggregate_function(function))
        {
            self.found = true;
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn contains_query_aggregate(query: &ast::Query) -> bool {
    let mut query = query.clone();
    let mut detector = QueryAggregateDetector {
        query_depth: 0,
        found: false,
    };
    let _ = query.visit(&mut detector);
    detector.found
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
fn validate_recursive_cte(query: &ast::Query, name: &str) -> Result<bool> {
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
fn create_set_expression_query(expression: ast::SetExpr) -> ast::Query {
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

impl ast::VisitorMut for CteForwardReferenceDetector<'_> {
    type Break = ();

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
        if self.names.contains(&name) && self.catalog.require_table(&name).is_err() {
            self.error = Some(PgError::create(
                SqlState::UndefinedTable,
                format!("relation {name:?} does not exist"),
            ));
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reject_cte_forward_references(
    query: &ast::Query,
    names: &[String],
    catalog: &Catalog,
) -> Result<()> {
    let mut query = query.clone();
    let mut detector = CteForwardReferenceDetector {
        catalog,
        names,
        error: None,
    };
    let _ = query.visit(&mut detector);
    detector.error.map_or(Ok(()), Err)
}

impl ast::VisitorMut for InlineCteReferenceReplacer<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.masked
            .push(self.pending_mask.take().unwrap_or_default());
        if query.with.is_none() {
            return std::ops::ControlFlow::Continue(());
        }
        match inline_query_ctes(query, self.state) {
            Ok(expanded) => *query = expanded,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
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
            name,
            alias,
            args: None,
            ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(name) = normalize_unqualified_object_name(name) else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(cte) = self.ctes.iter().rev().find(|cte| {
            cte.name == name && !self.masked.iter().any(|masked| masked.contains(&name))
        }) else {
            return std::ops::ControlFlow::Continue(());
        };
        let alias = match alias {
            Some(alias) if alias.columns.is_empty() => ast::TableAlias {
                columns: cte.alias.columns.clone(),
                ..alias.clone()
            },
            Some(alias) => alias.clone(),
            None => cte.alias.clone(),
        };
        *factor = ast::TableFactor::Derived {
            lateral: false,
            subquery: cte.query.clone(),
            alias: Some(alias),
            sample: None,
        };
        self.pending_mask = Some(cte.masked_names.clone());
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn inline_query_ctes(query: &ast::Query, state: &DatabaseState) -> Result<ast::Query> {
    let mut query = query.clone();
    let Some(with) = query.with.take() else {
        let mut replacer = InlineCteReferenceReplacer {
            state,
            ctes: &[],
            masked: Vec::new(),
            pending_mask: None,
            error: None,
        };
        let _ = query.visit(&mut replacer);
        if let Some(error) = replacer.error {
            return Err(error);
        }
        return Ok(query);
    };
    if with.recursive {
        return inline_recursive_query_ctes(query, with, state);
    }
    let names = with
        .cte_tables
        .iter()
        .map(|cte| normalize_identifier(&cte.alias.name))
        .collect::<Vec<_>>();
    let mut ctes = Vec::new();
    for (index, cte) in with.cte_tables.into_iter().enumerate() {
        let name = normalize_identifier(&cte.alias.name);
        if ctes
            .iter()
            .any(|existing: &InlineCte| existing.name == name)
        {
            return Err(PgError::create(
                SqlState::SyntaxError,
                format!("WITH query name {name:?} specified more than once"),
            ));
        }
        let mut cte_query = inline_query_ctes(&cte.query, state)?;
        reject_cte_forward_references(&cte_query, &names[index..], &state.catalog)?;
        let mut replacer = InlineCteReferenceReplacer {
            state,
            ctes: &ctes,
            masked: Vec::new(),
            pending_mask: None,
            error: None,
        };
        let _ = cte_query.visit(&mut replacer);
        if let Some(error) = replacer.error {
            return Err(error);
        }
        let mut alias = cte.alias;
        if is_data_modifying_query(&cte_query) {
            let statement = convert_query_to_statement(cte_query);
            let columns = describe_query_result_columns(state, &statement)?;
            if alias.columns.is_empty() {
                alias.columns = columns
                    .iter()
                    .map(|column| ast::TableAliasColumnDef {
                        name: ast::Ident::with_quote('"', column.name.clone()),
                        data_type: None,
                    })
                    .collect();
            }
            cte_query = create_cte_values_query(&QueryResult {
                columns,
                rows: Vec::new(),
            });
        }
        ctes.push(InlineCte {
            name,
            query: Box::new(cte_query),
            alias,
            masked_names: names[index..].to_vec(),
        });
    }
    let mut replacer = InlineCteReferenceReplacer {
        state,
        ctes: &ctes,
        masked: Vec::new(),
        pending_mask: None,
        error: None,
    };
    let _ = query.visit(&mut replacer);
    if let Some(error) = replacer.error {
        return Err(error);
    }
    Ok(query)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn inline_recursive_query_ctes(
    mut query: ast::Query,
    with: ast::With,
    state: &DatabaseState,
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
    let mut pending = with.cte_tables.into_iter().map(Some).collect::<Vec<_>>();
    let mut ctes = Vec::new();
    while ctes.len() < pending.len() {
        let mut progressed = false;
        for index in 0..pending.len() {
            let Some(cte) = pending[index].as_ref() else {
                continue;
            };
            let name = &names[index];
            let dependencies = collect_cte_references(&cte.query, &names);
            if dependencies.iter().any(|dependency| {
                dependency != name && !ctes.iter().any(|cte: &InlineCte| &cte.name == dependency)
            }) {
                continue;
            }
            let cte = pending[index]
                .take()
                .expect("pending CTE was checked as present");
            let mut cte_query = inline_query_ctes(&cte.query, state)?;
            let mut replacer = InlineCteReferenceReplacer {
                state,
                ctes: &ctes,
                masked: Vec::new(),
                pending_mask: None,
                error: None,
            };
            let _ = cte_query.visit(&mut replacer);
            if let Some(error) = replacer.error {
                return Err(error);
            }
            if validate_recursive_cte(&cte_query, name)? {
                let ast::SetExpr::SetOperation { left, .. } = cte_query.body.as_ref() else {
                    unreachable!("recursive CTE shape was validated");
                };
                let seed = InlineCte {
                    name: name.clone(),
                    query: Box::new(create_set_expression_query((**left).clone())),
                    alias: cte.alias.clone(),
                    masked_names: names.clone(),
                };
                let mut replacer = InlineCteReferenceReplacer {
                    state,
                    ctes: std::slice::from_ref(&seed),
                    masked: Vec::new(),
                    pending_mask: None,
                    error: None,
                };
                let _ = cte_query.visit(&mut replacer);
                if let Some(error) = replacer.error {
                    return Err(error);
                }
                validate_recursive_cte_types(&state.catalog, &cte_query)?;
            }
            ctes.push(InlineCte {
                name: name.clone(),
                query: Box::new(cte_query),
                alias: cte.alias,
                masked_names: names.clone(),
            });
            progressed = true;
        }
        if !progressed {
            return reject_unsupported("mutual recursion between WITH items is not implemented");
        }
    }
    let mut replacer = InlineCteReferenceReplacer {
        state,
        ctes: &ctes,
        masked: Vec::new(),
        pending_mask: None,
        error: None,
    };
    let _ = query.visit(&mut replacer);
    if let Some(error) = replacer.error {
        return Err(error);
    }
    Ok(query)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validate_recursive_cte_types(catalog: &Catalog, query: &ast::Query) -> Result<()> {
    if crate::analyzer::count_parameters(&ast::Statement::Query(Box::new(query.clone())))? != 0 {
        return Ok(());
    }
    let ast::SetExpr::SetOperation { left, right, .. } = query.body.as_ref() else {
        unreachable!("recursive CTE shape was validated");
    };
    let seed = super::scope::infer_query_output_columns(
        catalog,
        &create_set_expression_query((**left).clone()),
    )?;
    let recursive = super::scope::infer_query_output_columns(
        catalog,
        &create_set_expression_query((**right).clone()),
    )?;
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
pub(crate) fn expand_ctes_for_analysis(
    statement: &ast::Statement,
    state: &DatabaseState,
) -> Result<(ast::Statement, Vec<ast::Statement>)> {
    let ast::Statement::Query(query) = statement else {
        return Ok((statement.clone(), Vec::new()));
    };
    let mut mutations = Vec::new();
    if let Some(with) = &query.with
        && with.recursive
        && with
            .cte_tables
            .iter()
            .any(|cte| is_data_modifying_query(&cte.query))
    {
        let mut query = query.as_ref().clone();
        let with = query
            .with
            .take()
            .expect("WITH clause was checked as present");
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
        let mut pending = with.cte_tables.into_iter().map(Some).collect::<Vec<_>>();
        let mut ctes = Vec::new();
        while ctes.len() < pending.len() {
            let mut progressed = false;
            for index in 0..pending.len() {
                let Some(cte) = pending[index].as_ref() else {
                    continue;
                };
                let name = &names[index];
                let dependencies = collect_cte_references(&cte.query, &names);
                if dependencies.iter().any(|dependency| {
                    dependency != name
                        && !ctes.iter().any(|cte: &InlineCte| &cte.name == dependency)
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
                let mut cte_query = inline_query_ctes(&cte.query, state)?;
                let mut replacer = InlineCteReferenceReplacer {
                    state,
                    ctes: &ctes,
                    masked: Vec::new(),
                    pending_mask: None,
                    error: None,
                };
                let _ = cte_query.visit(&mut replacer);
                if let Some(error) = replacer.error {
                    return Err(error);
                }
                let mut alias = cte.alias;
                if modifying {
                    let mutation = convert_query_to_statement(cte_query.clone());
                    let columns = describe_query_result_columns(state, &mutation)?;
                    if alias.columns.is_empty() {
                        alias.columns = columns
                            .iter()
                            .map(|column| ast::TableAliasColumnDef {
                                name: ast::Ident::with_quote('"', column.name.clone()),
                                data_type: None,
                            })
                            .collect();
                    }
                    mutations.push(mutation);
                    cte_query = create_cte_values_query(&QueryResult {
                        columns,
                        rows: Vec::new(),
                    });
                } else if validate_recursive_cte(&cte_query, name)? {
                    let ast::SetExpr::SetOperation { left, .. } = cte_query.body.as_ref() else {
                        unreachable!("recursive CTE shape was validated");
                    };
                    let seed = InlineCte {
                        name: name.clone(),
                        query: Box::new(create_set_expression_query((**left).clone())),
                        alias: alias.clone(),
                        masked_names: names.clone(),
                    };
                    let mut replacer = InlineCteReferenceReplacer {
                        state,
                        ctes: std::slice::from_ref(&seed),
                        masked: Vec::new(),
                        pending_mask: None,
                        error: None,
                    };
                    let _ = cte_query.visit(&mut replacer);
                    if let Some(error) = replacer.error {
                        return Err(error);
                    }
                    validate_recursive_cte_types(&state.catalog, &cte_query)?;
                }
                ctes.push(InlineCte {
                    name: name.clone(),
                    query: Box::new(cte_query),
                    alias,
                    masked_names: names.clone(),
                });
                progressed = true;
            }
            if !progressed {
                return reject_unsupported(
                    "mutual recursion between WITH items is not implemented",
                );
            }
        }
        let mut replacer = InlineCteReferenceReplacer {
            state,
            ctes: &ctes,
            masked: Vec::new(),
            pending_mask: None,
            error: None,
        };
        let _ = query.visit(&mut replacer);
        if let Some(error) = replacer.error {
            return Err(error);
        }
        return Ok((convert_query_to_statement(query), mutations));
    }
    if let Some(with) = &query.with
        && !with.recursive
        && with
            .cte_tables
            .iter()
            .any(|cte| is_data_modifying_query(&cte.query))
    {
        let names = with
            .cte_tables
            .iter()
            .map(|cte| normalize_identifier(&cte.alias.name))
            .collect::<Vec<_>>();
        let mut ctes = Vec::new();
        for (index, cte) in with.cte_tables.iter().enumerate() {
            let name = normalize_identifier(&cte.alias.name);
            let mut cte_query = inline_query_ctes(&cte.query, state)?;
            reject_cte_forward_references(&cte_query, &names[index..], &state.catalog)?;
            let mut replacer = InlineCteReferenceReplacer {
                state,
                ctes: &ctes,
                masked: Vec::new(),
                pending_mask: None,
                error: None,
            };
            let _ = cte_query.visit(&mut replacer);
            if let Some(error) = replacer.error {
                return Err(error);
            }
            let mut alias = cte.alias.clone();
            if is_data_modifying_query(&cte_query) {
                let mutation = convert_query_to_statement(cte_query.clone());
                let columns = describe_query_result_columns(state, &mutation)?;
                if alias.columns.is_empty() {
                    alias.columns = columns
                        .iter()
                        .map(|column| ast::TableAliasColumnDef {
                            name: ast::Ident::with_quote('"', column.name.clone()),
                            data_type: None,
                        })
                        .collect();
                }
                mutations.push(mutation);
                cte_query = create_cte_values_query(&QueryResult {
                    columns,
                    rows: Vec::new(),
                });
            }
            ctes.push(InlineCte {
                name,
                query: Box::new(cte_query),
                alias,
                masked_names: names[index..].to_vec(),
            });
        }
    }
    Ok((
        convert_query_to_statement(inline_query_ctes(query, state)?),
        mutations,
    ))
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

struct CteReferenceCollector<'a> {
    names: &'a [String],
    masked: Vec<Vec<String>>,
    found: BTreeSet<String>,
}

impl ast::VisitorMut for CteReferenceCollector<'_> {
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
        if self.names.contains(&name) && !self.masked.iter().any(|masked| masked.contains(&name)) {
            self.found.insert(name);
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn collect_cte_references(query: &ast::Query, names: &[String]) -> BTreeSet<String> {
    let mut query = query.clone();
    let mut collector = CteReferenceCollector {
        names,
        masked: Vec::new(),
        found: BTreeSet::new(),
    };
    let _ = query.visit(&mut collector);
    collector.found
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_data_modifying_query(query: &ast::Query) -> bool {
    matches!(
        query.body.as_ref(),
        ast::SetExpr::Insert(_) | ast::SetExpr::Update(_) | ast::SetExpr::Delete(_)
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn collect_reachable_cte_names(query: &ast::Query) -> BTreeSet<String> {
    let Some(with) = &query.with else {
        return BTreeSet::new();
    };
    let names = with
        .cte_tables
        .iter()
        .map(|cte| normalize_identifier(&cte.alias.name))
        .collect::<Vec<_>>();
    let mut body = query.clone();
    body.with = None;
    let mut reachable = collect_cte_references(&body, &names);
    reachable.extend(
        with.cte_tables
            .iter()
            .filter(|cte| is_data_modifying_query(&cte.query))
            .map(|cte| normalize_identifier(&cte.alias.name)),
    );
    for (index, cte) in with.cte_tables.iter().enumerate().rev() {
        if reachable.contains(&names[index]) {
            reachable.extend(collect_cte_references(&cte.query, &names[..index]));
        }
    }
    reachable
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn has_zero_limit(query: &ast::Query) -> bool {
    matches!(
        &query.limit_clause,
        Some(ast::LimitClause::LimitOffset {
            limit: Some(ast::Expr::Value(value)),
            ..
        }) if matches!(&value.value, ast::Value::Number(number, _) if number == "0")
    )
}

struct CteReferenceReplacer<'a> {
    ctes: &'a [MaterializedCte],
    masked: Vec<Vec<String>>,
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

impl ast::VisitorMut for CteReferenceReplacer<'_> {
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
            name,
            alias,
            args: None,
            ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(name) = normalize_unqualified_object_name(name) else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(cte) = self.ctes.iter().rev().find(|cte| {
            cte.name == name && !self.masked.iter().any(|masked| masked.contains(&name))
        }) else {
            return std::ops::ControlFlow::Continue(());
        };
        let columns = if cte.alias.columns.is_empty() {
            cte.result
                .columns
                .iter()
                .map(|column| ast::TableAliasColumnDef {
                    name: ast::Ident::with_quote('"', column.name.clone()),
                    data_type: None,
                })
                .collect::<Vec<_>>()
        } else {
            cte.alias.columns.clone()
        };
        let alias = match alias {
            Some(alias) if alias.columns.is_empty() => ast::TableAlias {
                columns,
                ..alias.clone()
            },
            Some(alias) => alias.clone(),
            None => ast::TableAlias {
                name: cte.alias.name.clone(),
                columns,
                ..cte.alias.clone()
            },
        };
        *factor = ast::TableFactor::Derived {
            lateral: false,
            subquery: Box::new(create_cte_values_query(&cte.result)),
            alias: Some(alias),
            sample: None,
        };
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_cte_values_query(result: &QueryResult) -> ast::Query {
    let rows = if result.rows.is_empty() {
        let row = result
            .columns
            .iter()
            .map(|column| {
                crate::analyzer::create_typed_literal(
                    Value::Null,
                    PgType::create_with_typmod(
                        BaseType::resolve_oid(column.type_oid)
                            .expect("CTE result type OID is supported"),
                        column.typmod,
                    ),
                )
            })
            .collect::<Vec<_>>();
        let values = ast::Values {
            explicit_row: false,
            value_keyword: false,
            rows: vec![ast::Parens::with_empty_span(row.clone())],
        };
        ast::SetExpr::SetOperation {
            op: ast::SetOperator::Except,
            set_quantifier: ast::SetQuantifier::All,
            left: Box::new(ast::SetExpr::Values(values.clone())),
            right: Box::new(ast::SetExpr::Values(values)),
        }
    } else {
        ast::SetExpr::Values(ast::Values {
            explicit_row: false,
            value_keyword: false,
            rows: result
                .rows
                .iter()
                .map(|row| {
                    ast::Parens::with_empty_span(
                        row.iter()
                            .zip(&result.columns)
                            .map(|(value, column)| {
                                crate::analyzer::create_typed_literal(
                                    value.clone(),
                                    PgType::create_with_typmod(
                                        BaseType::resolve_oid(column.type_oid)
                                            .expect("CTE result type OID is supported"),
                                        column.typmod,
                                    ),
                                )
                            })
                            .collect(),
                    )
                })
                .collect(),
        })
    };
    ast::Query {
        with: None,
        body: Box::new(rows),
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
fn materialize_query_ctes(
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
        let _ = cte_query.visit(&mut CteReferenceReplacer {
            ctes: &ctes,
            masked: Vec::new(),
        });
        let result = if skips_rows {
            QueryResult {
                columns: describe_query_result_columns(
                    state,
                    &ast::Statement::Query(Box::new(cte_query.clone())),
                )?,
                rows: Vec::new(),
            }
        } else {
            let StatementResult::Query(result) =
                execute_query(state, &cte_query, xid, snapshot, context)?
            else {
                unreachable!("CTE query produces query rows");
            };
            result
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
    let _ = query.visit(&mut CteReferenceReplacer {
        ctes: &ctes,
        masked: Vec::new(),
    });
    Ok(query)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materialize_recursive_query_ctes(
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
            let _ = cte_query.visit(&mut CteReferenceReplacer {
                ctes: &ctes,
                masked: Vec::new(),
            });
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
                let StatementResult::Query(result) =
                    execute_query(state, &cte_query, xid, snapshot, context)?
                else {
                    unreachable!("CTE query produces query rows");
                };
                result
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
    let _ = query.visit(&mut CteReferenceReplacer {
        ctes: &ctes,
        masked: Vec::new(),
    });
    Ok(query)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materialize_recursive_data_modifying_ctes(
    state: &mut DatabaseState,
    mut query: ast::Query,
    with: ast::With,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
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
            let _ = cte_query.visit(&mut CteReferenceReplacer {
                ctes: &ctes,
                masked: Vec::new(),
            });
            let recursive = !modifying && validate_recursive_cte(&cte_query, name)?;
            let demand = if recursive {
                resolve_direct_cte_demand(&mut query, name, context)?
            } else {
                None
            };
            let mut result = if modifying {
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
                let StatementResult::Query(result) =
                    execute_query(state, &cte_query, xid, snapshot, context)?
                else {
                    unreachable!("CTE query produces query rows");
                };
                result
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
    let _ = query.visit(&mut CteReferenceReplacer {
        ctes: &ctes,
        masked: Vec::new(),
    });
    Ok(query)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_direct_cte_demand(
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
fn describe_recursive_cte_columns(
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
    let _ = recursive_query.visit(&mut CteReferenceReplacer {
        ctes: &[MaterializedCte {
            name: name.to_owned(),
            alias: alias.clone(),
            result: QueryResult {
                columns: seed_columns.clone(),
                rows: Vec::new(),
            },
        }],
        masked: Vec::new(),
    });
    let recursive_columns =
        describe_query_result_columns(state, &ast::Statement::Query(Box::new(recursive_query)))?;
    resolve_recursive_columns(&seed_columns, &recursive_columns)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_recursive_cte(
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
    let mut recursive_query = create_set_expression_query((**right).clone());
    let _ = recursive_query.visit(&mut CteReferenceReplacer {
        ctes: &[MaterializedCte {
            name: name.to_owned(),
            alias: alias.clone(),
            result: QueryResult {
                columns: seed.columns.clone(),
                rows: Vec::new(),
            },
        }],
        masked: Vec::new(),
    });
    let recursive_columns =
        describe_query_result_columns(state, &ast::Statement::Query(Box::new(recursive_query)))?;
    let columns = resolve_recursive_columns(&seed.columns, &recursive_columns)?;
    let distinct = matches!(
        set_quantifier,
        ast::SetQuantifier::None | ast::SetQuantifier::Distinct
    );
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
        let _ = recursive_query.visit(&mut CteReferenceReplacer {
            ctes: &[MaterializedCte {
                name: name.to_owned(),
                alias: alias.clone(),
                result: QueryResult {
                    columns: columns.clone(),
                    rows: working,
                },
            }],
            masked: Vec::new(),
        });
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
                    if compare_group_keys(existing, &candidate)? {
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn materialize_ctes(
    state: &mut DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
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
        let _ = cte_query.visit(&mut CteReferenceReplacer {
            ctes: &ctes,
            masked: Vec::new(),
        });
        let result = if modifying {
            cte_query = materialize_query_ctes(state, &cte_query, xid, snapshot, context)?;
            let cte_statement = convert_query_to_statement(cte_query);
            let cte_statement =
                materialize_uncorrelated_subqueries(state, &cte_statement, xid, snapshot, context)?;
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
        } else if skips_rows && !mutation_required.contains(&name) {
            QueryResult {
                columns: describe_query_result_columns(
                    state,
                    &ast::Statement::Query(Box::new(cte_query.clone())),
                )?,
                rows: Vec::new(),
            }
        } else {
            let StatementResult::Query(result) =
                execute_query(state, &cte_query, xid, snapshot, context)?
            else {
                unreachable!("CTE query produces query rows");
            };
            result
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
    let _ = query.visit(&mut CteReferenceReplacer {
        ctes: &ctes,
        masked: Vec::new(),
    });
    Ok(convert_query_to_statement(materialize_query_ctes(
        state, &query, xid, snapshot, context,
    )?))
}

struct SubqueryDetector {
    found: bool,
}

struct StatementFeatureDetector {
    cte: bool,
    subquery: bool,
}

impl ast::Visitor for StatementFeatureDetector {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.cte |= query.with.is_some();
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expr: &ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        self.subquery |= matches!(
            expr,
            ast::Expr::Subquery(_) | ast::Expr::Exists { .. } | ast::Expr::InSubquery { .. }
        ) || matches!(
            expr,
            ast::Expr::AnyOp { right, .. } | ast::Expr::AllOp { right, .. }
                if matches!(right.as_ref(), ast::Expr::Subquery(_))
        );
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn detect_statement_features(statement: &ast::Statement) -> (bool, bool) {
    let mut detector = StatementFeatureDetector {
        cte: false,
        subquery: false,
    };
    let _ = ast::Visit::visit(statement, &mut detector);
    (detector.cte, detector.subquery)
}

impl ast::Visitor for SubqueryDetector {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, _query: &ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.found = true;
        std::ops::ControlFlow::Break(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn contains_subquery(expression: &ast::Expr) -> bool {
    let mut detector = SubqueryDetector { found: false };
    let _ = ast::Visit::visit(expression, &mut detector);
    detector.found
}

struct SubqueryMaterializer<'a> {
    state: &'a DatabaseState,
    xid: Xid,
    snapshot: &'a Snapshot,
    context: &'a StatementExecutionContext,
    error: Option<PgError>,
    defer_unresolved: bool,
    scopes: Vec<BoundScope>,
}

impl SubqueryMaterializer<'_> {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn execute(&self, query: &ast::Query) -> Result<QueryResult> {
        let query = materialize_uncorrelated_subqueries(
            self.state,
            &ast::Statement::Query(Box::new(query.clone())),
            self.xid,
            self.snapshot,
            self.context,
        )?;
        let ast::Statement::Query(query) = query else {
            unreachable!("subquery statement remains a query");
        };
        let StatementResult::Query(result) =
            execute_query(self.state, &query, self.xid, self.snapshot, self.context)?
        else {
            unreachable!("subquery execution returns query rows");
        };
        Ok(result)
    }
}

impl ast::VisitorMut for SubqueryMaterializer<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        let scope = match query.body.as_ref() {
            ast::SetExpr::Select(select) => bind_query_scope(&self.state.catalog, select),
            _ => Ok(BoundScope {
                columns: Vec::new(),
            }),
        }
        .unwrap_or(BoundScope {
            columns: Vec::new(),
        });
        self.scopes.push(scope);
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.scopes.pop().expect("visited query pushed a scope");
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expr: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if !matches!(
            expr,
            ast::Expr::AnyOp { right, .. } | ast::Expr::AllOp { right, .. }
                if matches!(right.as_ref(), ast::Expr::Subquery(_))
        ) && !matches!(
            expr,
            ast::Expr::Subquery(_) | ast::Expr::Exists { .. } | ast::Expr::InSubquery { .. }
        ) {
            return std::ops::ControlFlow::Continue(());
        }
        let original = expr.clone();
        let correlation_candidate = original.clone();
        let result = (|| match original {
            ast::Expr::AnyOp {
                left,
                compare_op,
                right,
                is_some,
            } => {
                let ast::Expr::Subquery(subquery) = right.as_ref() else {
                    return Ok(None);
                };
                let result = self.execute(subquery)?;
                if result.columns.len() != 1 {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let data_type = PgType::create_with_typmod(
                    BaseType::resolve_oid(result.columns[0].type_oid)
                        .expect("query result type OID is supported"),
                    result.columns[0].typmod,
                );
                Ok(Some(ast::Expr::AnyOp {
                    left,
                    compare_op,
                    right: Box::new(ast::Expr::Tuple(
                        result
                            .rows
                            .into_iter()
                            .map(|row| {
                                crate::analyzer::create_typed_literal(row[0].clone(), data_type)
                            })
                            .collect(),
                    )),
                    is_some,
                }))
            }
            ast::Expr::AllOp {
                left,
                compare_op,
                right,
            } => {
                let ast::Expr::Subquery(subquery) = right.as_ref() else {
                    return Ok(None);
                };
                let result = self.execute(subquery)?;
                if result.columns.len() != 1 {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let data_type = PgType::create_with_typmod(
                    BaseType::resolve_oid(result.columns[0].type_oid)
                        .expect("query result type OID is supported"),
                    result.columns[0].typmod,
                );
                Ok(Some(ast::Expr::AllOp {
                    left,
                    compare_op,
                    right: Box::new(ast::Expr::Tuple(
                        result
                            .rows
                            .into_iter()
                            .map(|row| {
                                crate::analyzer::create_typed_literal(row[0].clone(), data_type)
                            })
                            .collect(),
                    )),
                }))
            }
            ast::Expr::Subquery(query) => {
                let result = self.execute(&query)?;
                if result.columns.len() != 1 {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery must return only one column",
                    ));
                }
                if result.rows.len() > 1 {
                    return Err(PgError::create(
                        SqlState::CardinalityViolation,
                        "more than one row returned by a subquery used as an expression",
                    ));
                }
                let data_type = PgType::create_with_typmod(
                    BaseType::resolve_oid(result.columns[0].type_oid)
                        .expect("query result type OID is supported"),
                    result.columns[0].typmod,
                );
                Ok(Some(crate::analyzer::create_typed_literal(
                    result
                        .rows
                        .into_iter()
                        .next()
                        .map(|row| row[0].clone())
                        .unwrap_or(Value::Null),
                    data_type,
                )))
            }
            ast::Expr::Exists { subquery, negated } => {
                Ok(Some(crate::analyzer::create_typed_literal(
                    Value::Bool(self.execute(&subquery)?.rows.is_empty() == negated),
                    PgType::create(BaseType::Bool),
                )))
            }
            ast::Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => {
                let result = self.execute(&subquery)?;
                let left_width = match expr.as_ref() {
                    ast::Expr::Tuple(fields) => fields.len(),
                    _ => 1,
                };
                if result.columns.len() != left_width {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let types = result
                    .columns
                    .iter()
                    .map(|column| {
                        PgType::create_with_typmod(
                            BaseType::resolve_oid(column.type_oid)
                                .expect("query result type OID is supported"),
                            column.typmod,
                        )
                    })
                    .collect::<Vec<_>>();
                Ok(Some(ast::Expr::InList {
                    expr,
                    list: result
                        .rows
                        .into_iter()
                        .map(|row| {
                            let fields = row
                                .into_iter()
                                .zip(&types)
                                .map(|(value, data_type)| {
                                    crate::analyzer::create_typed_literal(value, *data_type)
                                })
                                .collect::<Vec<_>>();
                            if fields.len() == 1 {
                                fields.into_iter().next().expect("row has one field")
                            } else {
                                ast::Expr::Tuple(fields)
                            }
                        })
                        .collect(),
                    negated,
                }))
            }
            _ => Ok(None),
        })();
        match result {
            Ok(Some(value)) => *expr = value,
            Ok(None) => {}
            Err(error)
                if self.defer_unresolved
                    && matches!(
                        error.sqlstate,
                        SqlState::UndefinedColumn | SqlState::UndefinedTable
                    ) =>
            {
                match self.scopes.last().map(|outer| {
                    references_outer_scope(&self.state.catalog, &correlation_candidate, outer)
                }) {
                    Some(Ok(true)) => {}
                    Some(Err(scope_error)) => self.error = Some(scope_error),
                    _ => self.error = Some(error),
                }
            }
            Err(error) => self.error = Some(error),
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn materialize_uncorrelated_subqueries(
    state: &DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<ast::Statement> {
    let scope = match statement {
        ast::Statement::Insert(insert) => {
            let schema = state
                .catalog
                .require_table(&resolve_insert_table_name(&insert.table)?)?;
            Some(bind_target_scope(
                schema,
                insert.table_alias.as_ref().map(|alias| &alias.alias),
            ))
        }
        ast::Statement::Update(update) => {
            let ast::TableFactor::Table { name, alias, .. } = &update.table.relation else {
                return Ok(statement.clone());
            };
            let schema = state
                .catalog
                .require_table(&normalize_unqualified_object_name(name)?)?;
            let from = match &update.from {
                None => &[][..],
                Some(ast::UpdateTableFromKind::AfterSet(from)) => from.as_slice(),
                Some(ast::UpdateTableFromKind::BeforeSet(_)) => &[][..],
            };
            Some(combine_bound_scopes(
                bind_target_scope(schema, alias.as_ref().map(|alias| &alias.name)),
                bind_from_scope(&state.catalog, from)?,
            ))
        }
        ast::Statement::Delete(delete) => {
            let ast::FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(statement.clone());
            };
            let Some(ast::TableWithJoins {
                relation: ast::TableFactor::Table { name, alias, .. },
                ..
            }) = from.first()
            else {
                return Ok(statement.clone());
            };
            let schema = state
                .catalog
                .require_table(&normalize_unqualified_object_name(name)?)?;
            Some(combine_bound_scopes(
                bind_target_scope(schema, alias.as_ref().map(|alias| &alias.name)),
                bind_from_scope(&state.catalog, delete.using.as_deref().unwrap_or_default())?,
            ))
        }
        _ => None,
    };
    let mut statement = statement.clone();
    materialize_subqueries(
        state,
        &mut statement,
        xid,
        snapshot,
        context,
        true,
        scope.into_iter().collect(),
    )?;
    Ok(statement)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materialize_subqueries<V: ast::VisitMut>(
    state: &DatabaseState,
    value: &mut V,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    defer_unresolved: bool,
    scopes: Vec<BoundScope>,
) -> Result<()> {
    let mut materializer = SubqueryMaterializer {
        state,
        xid,
        snapshot,
        context,
        error: None,
        defer_unresolved,
        scopes,
    };
    let _ = value.visit(&mut materializer);
    if let Some(error) = materializer.error {
        return Err(error);
    }
    Ok(())
}

struct OuterReferenceSubstituter<'a> {
    catalog: &'a Catalog,
    outer_scope: &'a BoundScope,
    outer_row: &'a [Value],
    scopes: Vec<BoundScope>,
    error: Option<PgError>,
    substituted: bool,
}

impl ast::VisitorMut for OuterReferenceSubstituter<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        let ast::SetExpr::Select(select) = query.body.as_ref() else {
            self.scopes.push(BoundScope {
                columns: Vec::new(),
            });
            return std::ops::ControlFlow::Continue(());
        };
        match bind_query_scope(self.catalog, select) {
            Ok(scope) => self.scopes.push(scope),
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.scopes.pop().expect("visited query pushed a scope");
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        let identifiers = match expression {
            ast::Expr::Identifier(identifier) => std::slice::from_ref(identifier),
            ast::Expr::CompoundIdentifier(identifiers) => identifiers.as_slice(),
            _ => return std::ops::ControlFlow::Continue(()),
        };
        for scope in self.scopes.iter().rev() {
            if identifiers.len() == 2 {
                let qualifier = normalize_identifier(&identifiers[0]);
                if !scope
                    .columns
                    .iter()
                    .any(|column| column.qualifier == qualifier)
                {
                    continue;
                }
            }
            match scope.resolve_column(identifiers) {
                Ok(_) => return std::ops::ControlFlow::Continue(()),
                Err(error)
                    if identifiers.len() == 1 && error.sqlstate == SqlState::UndefinedColumn => {}
                Err(error) => {
                    self.error = Some(error);
                    return std::ops::ControlFlow::Break(());
                }
            }
        }
        match self.outer_scope.resolve_column(identifiers) {
            Ok((_, data_type)) => {
                match RowScope::Bound(self.outer_scope)
                    .resolve_column_value(identifiers, self.outer_row)
                {
                    Ok(value) => {
                        *expression = crate::analyzer::create_typed_literal(value, data_type);
                        self.substituted = true;
                    }
                    Err(error) => {
                        self.error = Some(error);
                        return std::ops::ControlFlow::Break(());
                    }
                }
            }
            Err(error)
                if matches!(
                    error.sqlstate,
                    SqlState::UndefinedColumn | SqlState::UndefinedTable
                ) => {}
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn references_outer_scope(
    catalog: &Catalog,
    expression: &ast::Expr,
    outer_scope: &BoundScope,
) -> Result<bool> {
    let mut expression = expression.clone();
    let outer_row = vec![Value::Null; outer_scope.columns.len()];
    let mut substituter = OuterReferenceSubstituter {
        catalog,
        outer_scope,
        outer_row: &outer_row,
        scopes: Vec::new(),
        error: None,
        substituted: false,
    };
    let _ = expression.visit(&mut substituter);
    substituter.error.map_or(Ok(substituter.substituted), Err)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_query_expression(
    state: &DatabaseState,
    expression: &ast::Expr,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Value> {
    if !contains_subquery(expression) {
        return evaluate(expression, RowScope::Bound(scope), row, context);
    }
    let mut expression = expression.clone();
    let mut substituter = OuterReferenceSubstituter {
        catalog: &state.catalog,
        outer_scope: scope,
        outer_row: row,
        scopes: Vec::new(),
        error: None,
        substituted: false,
    };
    let _ = expression.visit(&mut substituter);
    if let Some(error) = substituter.error {
        return Err(error);
    }
    materialize_subqueries(
        state,
        &mut expression,
        xid,
        snapshot,
        context,
        false,
        Vec::new(),
    )?;
    evaluate(&expression, RowScope::Bound(scope), row, context)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn describe_query_result_columns(
    state: &DatabaseState,
    statement: &ast::Statement,
) -> Result<Vec<ColumnMeta>> {
    match statement {
        ast::Statement::Query(query) => match query.body.as_ref() {
            ast::SetExpr::Select(select) => bind_select_scope(state, select).and_then(|scope| {
                build_projection_plan(state, &select.projection, &scope).map(|(_, columns)| columns)
            }),
            ast::SetExpr::Values(values) => bind_values_scope(values).map(|scope| {
                scope
                    .columns
                    .iter()
                    .map(|column| ColumnMeta {
                        name: column.name.clone(),
                        type_oid: column.data_type.map_to_oid(),
                        typmod: column.data_type.typmod,
                    })
                    .collect()
            }),
            _ => describe_set_expression_columns(state, query, &query.body),
        },
        ast::Statement::Insert(insert) => {
            let Some(returning) = &insert.returning else {
                return Ok(Vec::new());
            };
            let schema = state
                .catalog
                .require_table(&resolve_insert_table_name(&insert.table)?)?;
            let scope = bind_target_scope(
                schema,
                insert.table_alias.as_ref().map(|alias| &alias.alias),
            );
            build_mutation_projection_plan(state, returning, &scope, schema.columns.len())
                .map(|(_, columns)| columns)
        }
        ast::Statement::Update(update) => {
            let Some(returning) = &update.returning else {
                return Ok(Vec::new());
            };
            let ast::TableFactor::Table {
                name, alias, args, ..
            } = &update.table.relation
            else {
                return Ok(Vec::new());
            };
            if args.is_some() {
                return Ok(Vec::new());
            }
            let schema = state
                .catalog
                .require_table(&normalize_unqualified_object_name(name)?)?;
            let from = match &update.from {
                None => &[][..],
                Some(ast::UpdateTableFromKind::AfterSet(from)) => from.as_slice(),
                Some(ast::UpdateTableFromKind::BeforeSet(_)) => return Ok(Vec::new()),
            };
            let scope = combine_bound_scopes(
                bind_target_scope(schema, alias.as_ref().map(|alias| &alias.name)),
                bind_from_scope(&state.catalog, from)?,
            );
            build_mutation_projection_plan(state, returning, &scope, schema.columns.len())
                .map(|(_, columns)| columns)
        }
        ast::Statement::Delete(delete) => {
            let Some(returning) = &delete.returning else {
                return Ok(Vec::new());
            };
            let ast::FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(Vec::new());
            };
            let Some(ast::TableWithJoins {
                relation:
                    ast::TableFactor::Table {
                        name, alias, args, ..
                    },
                ..
            }) = from.first()
            else {
                return Ok(Vec::new());
            };
            if args.is_some() {
                return Ok(Vec::new());
            }
            let schema = state
                .catalog
                .require_table(&normalize_unqualified_object_name(name)?)?;
            let scope = combine_bound_scopes(
                bind_target_scope(schema, alias.as_ref().map(|alias| &alias.name)),
                bind_from_scope(&state.catalog, delete.using.as_deref().unwrap_or_default())?,
            );
            build_mutation_projection_plan(state, returning, &scope, schema.columns.len())
                .map(|(_, columns)| columns)
        }
        _ => Ok(Vec::new()),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn describe_set_expression_columns(
    state: &DatabaseState,
    query: &ast::Query,
    expression: &ast::SetExpr,
) -> Result<Vec<ColumnMeta>> {
    match expression {
        ast::SetExpr::Query(query) => {
            describe_query_result_columns(state, &ast::Statement::Query(query.clone()))
        }
        ast::SetExpr::SetOperation { left, right, .. } => resolve_set_columns(
            &describe_set_expression_columns(state, query, left)?,
            &describe_set_expression_columns(state, query, right)?,
        ),
        ast::SetExpr::Select(_) | ast::SetExpr::Values(_) => {
            let mut operand = query.clone();
            operand.with = None;
            operand.body = Box::new(expression.clone());
            operand.order_by = None;
            operand.limit_clause = None;
            operand.fetch = None;
            operand.locks.clear();
            describe_query_result_columns(state, &ast::Statement::Query(Box::new(operand)))
        }
        _ => reject_unsupported("set-operation input is not implemented"),
    }
}
pub(super) enum ProjectionSource<'a> {
    Column(usize),
    Merged(usize, usize, PgType),
    Expression(&'a ast::Expr),
}
enum OrderKey<'a> {
    Output(usize),
    Input(usize),
    Expression(&'a ast::Expr),
}
enum DistinctPlan<'a> {
    None,
    Rows,
    On {
        expressions: &'a [ast::Expr],
        keys: Vec<DistinctKey<'a>>,
    },
}
enum DistinctKey<'a> {
    Output(usize),
    Order(usize),
    Expression(&'a ast::Expr),
}
enum RowCountClause {
    Limit,
    Offset,
}
struct RowOrderSpec<'a> {
    key: OrderKey<'a>,
    ascending: bool,
    nulls_first: bool,
}
struct GroupingPlan {
    expressions: Vec<(ast::Expr, PgType)>,
    enabled: bool,
}
struct OrderedRow {
    values: Vec<Value>,
    keys: Vec<Value>,
    distinct_keys: Vec<Value>,
}
#[derive(Default)]
struct AggregateUsage {
    found: bool,
    outside_column: bool,
}
struct AggregateValidator<'a> {
    scope: &'a BoundScope,
    query_depth: usize,
    aggregate_depth: usize,
    usage: AggregateUsage,
    error: Option<PgError>,
}
struct AggregateMaterializer<'a> {
    state: &'a DatabaseState,
    scope: &'a BoundScope,
    rows: &'a [Vec<Value>],
    xid: Xid,
    snapshot: &'a Snapshot,
    context: &'a StatementExecutionContext,
    query_depth: usize,
    error: Option<PgError>,
}
struct GroupedExpressionSubstituter<'a> {
    catalog: &'a crate::catalog::Catalog,
    grouped_expressions: &'a [(ast::Expr, PgType)],
    grouped_columns: &'a [(usize, PgType)],
    scope: &'a BoundScope,
    scopes: Vec<BoundScope>,
    query_depth: usize,
    aggregate_depth: usize,
    error: Option<PgError>,
}
struct BoundExpressionNormalizer<'a> {
    scope: &'a BoundScope,
    query_depth: usize,
    error: Option<PgError>,
}
#[derive(Eq, Hash, PartialEq)]
enum JoinKey {
    Bool(bool),
    Int2(i16),
    Int4(i32),
    Int8(i64),
    Text(String),
    Bytea(Vec<u8>),
    Uuid(uuid::Uuid),
}

impl ast::VisitorMut for AggregateValidator<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth -= 1;
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if self.query_depth != 0 {
            return std::ops::ControlFlow::Continue(());
        }
        if let ast::Expr::Function(function) = expression
            && is_aggregate_function(function)
        {
            if self.aggregate_depth != 0 {
                self.error = Some(PgError::create(
                    SqlState::GroupingError,
                    "aggregate function calls cannot be nested",
                ));
                return std::ops::ControlFlow::Break(());
            }
            if let Err(error) = infer_aggregate_return_type(function, RowScope::Bound(self.scope)) {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
            self.usage.found = true;
            self.aggregate_depth += 1;
        } else if self.aggregate_depth == 0
            && matches!(
                expression,
                ast::Expr::Identifier(_) | ast::Expr::CompoundIdentifier(_)
            )
        {
            self.usage.outside_column = true;
        }
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_expr(
        &mut self,
        expression: &mut ast::Expr,
    ) -> std::ops::ControlFlow<Self::Break> {
        if self.query_depth == 0
            && matches!(expression, ast::Expr::Function(function) if is_aggregate_function(function))
        {
            self.aggregate_depth -= 1;
        }
        std::ops::ControlFlow::Continue(())
    }
}

impl ast::VisitorMut for AggregateMaterializer<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth -= 1;
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if self.query_depth != 0 {
            return std::ops::ControlFlow::Continue(());
        }
        let ast::Expr::Function(function) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        if !is_aggregate_function(function) {
            return std::ops::ControlFlow::Continue(());
        }
        let original_argument = match &function.args {
            ast::FunctionArguments::List(arguments) => match arguments.args.as_slice() {
                [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))] => Some(argument),
                _ => None,
            },
            _ => None,
        };
        let original_filter = function.filter.as_deref();
        let typed_expression = match super::scope::substitute_typed_subqueries(
            &self.state.catalog,
            &ast::Expr::Function(function.clone()),
            self.scope,
        ) {
            Ok(expression) => expression,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        let ast::Expr::Function(typed_function) = typed_expression else {
            unreachable!("typed aggregate expression remains a function")
        };
        match evaluate_aggregate_function(
            &typed_function,
            RowScope::Bound(self.scope),
            self.rows,
            |typed_expression, row| {
                let expression = if typed_function
                    .filter
                    .as_deref()
                    .is_some_and(|filter| std::ptr::eq(filter, typed_expression))
                {
                    original_filter.expect("aggregate FILTER expression was validated")
                } else {
                    original_argument.expect("aggregate expression argument was validated")
                };
                evaluate_query_expression(
                    self.state,
                    expression,
                    self.scope,
                    row,
                    self.xid,
                    self.snapshot,
                    self.context,
                )
            },
        ) {
            Ok((value, data_type)) => {
                *expression =
                    crate::analyzer::create_typed_literal(value, PgType::create(data_type));
                std::ops::ControlFlow::Continue(())
            }
            Err(error) => {
                self.error = Some(error);
                std::ops::ControlFlow::Break(())
            }
        }
    }
}

impl ast::VisitorMut for GroupedExpressionSubstituter<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        let outer = self
            .scopes
            .last()
            .expect("grouped validator has a root scope");
        let scope = match query.body.as_ref() {
            ast::SetExpr::Select(select) => {
                super::scope::bind_query_scope_with_outer(self.catalog, select, outer)
            }
            _ => Ok(outer.clone()),
        };
        match scope {
            Ok(scope) => self.scopes.push(scope),
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        self.query_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth -= 1;
        self.scopes.pop().expect("visited query pushed a scope");
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if self.query_depth != 0 {
            if self.aggregate_depth != 0 {
                return std::ops::ControlFlow::Continue(());
            }
            let identifiers = match expression {
                ast::Expr::Identifier(identifier) => Some(std::slice::from_ref(identifier)),
                ast::Expr::CompoundIdentifier(identifiers) => Some(identifiers.as_slice()),
                _ => None,
            };
            let Some(identifiers) = identifiers else {
                return std::ops::ControlFlow::Continue(());
            };
            let scope = self.scopes.last().expect("nested query has a bound scope");
            let (slot, _) = match scope.resolve_column(identifiers) {
                Ok(resolved) => resolved,
                Err(error) => {
                    self.error = Some(error);
                    return std::ops::ControlFlow::Break(());
                }
            };
            let column = scope
                .columns
                .iter()
                .find(|column| column.slot == slot)
                .expect("resolved nested column is in its scope");
            if column.depth == 0 {
                return std::ops::ControlFlow::Continue(());
            }
            let grouped = self.grouped_columns.iter().any(|(slot, _)| {
                self.scope.columns.iter().any(|root| {
                    root.slot == *slot
                        && root.table_id == column.table_id
                        && root.qualifier == column.qualifier
                        && root.source_name == column.source_name
                })
            });
            if !grouped {
                self.error = Some(PgError::create(
                    SqlState::GroupingError,
                    "subquery uses ungrouped column from outer query",
                ));
                return std::ops::ControlFlow::Break(());
            }
            return std::ops::ControlFlow::Continue(());
        }
        if matches!(expression, ast::Expr::Function(function) if is_aggregate_function(function)) {
            self.aggregate_depth += 1;
            return std::ops::ControlFlow::Continue(());
        }
        if self.aggregate_depth != 0 {
            return std::ops::ControlFlow::Continue(());
        }
        if let Some((_, data_type)) = self
            .grouped_expressions
            .iter()
            .find(|(grouped, _)| grouped == expression)
        {
            *expression = crate::analyzer::create_typed_literal(Value::Null, *data_type);
            return std::ops::ControlFlow::Continue(());
        }
        let identifiers = match expression {
            ast::Expr::Identifier(identifier) => Some(std::slice::from_ref(identifier)),
            ast::Expr::CompoundIdentifier(identifiers) => Some(identifiers.as_slice()),
            _ => None,
        };
        if let Some(identifiers) = identifiers {
            match self.scope.resolve_column(identifiers) {
                Ok((slot, _)) => {
                    if let Some((_, data_type)) = self
                        .grouped_columns
                        .iter()
                        .find(|(grouped, _)| *grouped == slot)
                    {
                        *expression =
                            crate::analyzer::create_typed_literal(Value::Null, *data_type);
                    }
                }
                Err(error) => {
                    self.error = Some(error);
                    return std::ops::ControlFlow::Break(());
                }
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_expr(
        &mut self,
        expression: &mut ast::Expr,
    ) -> std::ops::ControlFlow<Self::Break> {
        if self.query_depth == 0
            && matches!(expression, ast::Expr::Function(function) if is_aggregate_function(function))
        {
            self.aggregate_depth -= 1;
        }
        std::ops::ControlFlow::Continue(())
    }
}

impl ast::VisitorMut for BoundExpressionNormalizer<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth -= 1;
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if self.query_depth != 0 {
            return std::ops::ControlFlow::Continue(());
        }
        let identifiers = match expression {
            ast::Expr::Identifier(identifier) => Some(std::slice::from_ref(identifier)),
            ast::Expr::CompoundIdentifier(identifiers) => Some(identifiers.as_slice()),
            _ => None,
        };
        let Some(identifiers) = identifiers else {
            return std::ops::ControlFlow::Continue(());
        };
        match self.scope.resolve_column(identifiers) {
            Ok((slot, _)) => {
                *expression =
                    ast::Expr::Identifier(ast::Ident::new(format!("__pg_fake_bound_{slot}")));
            }
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn normalize_bound_expression(expression: &ast::Expr, scope: &BoundScope) -> Result<ast::Expr> {
    let mut expression = expression.clone();
    let mut normalizer = BoundExpressionNormalizer {
        scope,
        query_depth: 0,
        error: None,
    };
    let _ = expression.visit(&mut normalizer);
    normalizer.error.map_or(Ok(expression), Err)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn compare_bound_expressions(
    left: &ast::Expr,
    right: &ast::Expr,
    scope: &BoundScope,
) -> Result<bool> {
    Ok(normalize_bound_expression(left, scope)? == normalize_bound_expression(right, scope)?)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn inspect_aggregate_usage(
    state: &DatabaseState,
    expression: &ast::Expr,
    scope: &BoundScope,
) -> Result<AggregateUsage> {
    let mut expression =
        super::scope::substitute_typed_subqueries(&state.catalog, expression, scope)?;
    let mut validator = AggregateValidator {
        scope,
        query_depth: 0,
        aggregate_depth: 0,
        usage: AggregateUsage::default(),
        error: None,
    };
    let _ = expression.visit(&mut validator);
    validator.error.map_or(Ok(validator.usage), Err)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materialize_aggregate_expression(
    state: &DatabaseState,
    expression: &ast::Expr,
    scope: &BoundScope,
    rows: &[Vec<Value>],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<ast::Expr> {
    let mut expression = expression.clone();
    let mut materializer = AggregateMaterializer {
        state,
        scope,
        rows,
        xid,
        snapshot,
        context,
        query_depth: 0,
        error: None,
    };
    let _ = expression.visit(&mut materializer);
    materializer.error.map_or(Ok(expression), Err)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validate_grouped_expression(
    state: &DatabaseState,
    expression: &ast::Expr,
    scope: &BoundScope,
    grouped_expressions: &[(ast::Expr, PgType)],
    grouped_columns: &[(usize, PgType)],
) -> Result<bool> {
    let mut expression = expression.clone();
    let mut substituter = GroupedExpressionSubstituter {
        catalog: &state.catalog,
        grouped_expressions,
        grouped_columns,
        scope,
        scopes: vec![scope.clone()],
        query_depth: 0,
        aggregate_depth: 0,
        error: None,
    };
    let _ = expression.visit(&mut substituter);
    if let Some(error) = substituter.error {
        return Err(error);
    }
    let usage = inspect_aggregate_usage(state, &expression, scope)?;
    if usage.outside_column {
        return Err(PgError::create(
            SqlState::GroupingError,
            "column must appear in the GROUP BY clause or be used in an aggregate function",
        ));
    }
    Ok(usage.found)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn compare_group_keys(left: &[Value], right: &[Value]) -> Result<bool> {
    assert_eq!(left.len(), right.len());
    for (left, right) in left.iter().zip(right) {
        match (left, right) {
            (Value::Null, Value::Null) => {}
            (Value::Null, _) | (_, Value::Null) => return Ok(false),
            _ if compare_values(left, right)? == Ordering::Equal => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_projection_expression(
    projection: &ProjectionSource<'_>,
    scope: &BoundScope,
) -> ast::Expr {
    match projection {
        ProjectionSource::Expression(expression) => (*expression).clone(),
        ProjectionSource::Column(slot) => {
            let column = scope
                .columns
                .iter()
                .find(|column| column.slot == *slot && column.wildcard)
                .expect("projected column is present in the bound scope");
            ast::Expr::CompoundIdentifier(vec![
                ast::Ident::new(column.qualifier.clone()),
                ast::Ident::new(column.name.clone()),
            ])
        }
        ProjectionSource::Merged(left, right, _) => {
            let column = scope
                .columns
                .iter()
                .find(|column| column.merged == Some((*left, *right)) && column.wildcard)
                .expect("projected merged column is present in the bound scope");
            ast::Expr::Identifier(ast::Ident::new(column.name.clone()))
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_grouping_expressions(
    state: &DatabaseState,
    expressions: &[ast::Expr],
    projections: &[ProjectionSource<'_>],
    columns: &[ColumnMeta],
    scope: &BoundScope,
) -> Result<Vec<(ast::Expr, PgType)>> {
    expressions
        .iter()
        .map(|expression| {
            let resolved = if let Some(position) = extract_number_literal(expression)
                && !position.contains(['.', 'e', 'E'])
            {
                let position = position.parse::<usize>().map_err(|_| {
                    PgError::create(
                        SqlState::InvalidColumnReference,
                        "GROUP BY position is not in select list",
                    )
                })?;
                if position == 0 || position > projections.len() {
                    return Err(PgError::create(
                        SqlState::InvalidColumnReference,
                        "GROUP BY position is not in select list",
                    ));
                }
                create_projection_expression(&projections[position - 1], scope)
            } else if let ast::Expr::Identifier(identifier) = expression {
                match scope.resolve_column(std::slice::from_ref(identifier)) {
                    Ok(_) => expression.clone(),
                    Err(error) if error.sqlstate == SqlState::UndefinedColumn => {
                        let name = normalize_identifier(identifier);
                        let matches = columns
                            .iter()
                            .enumerate()
                            .filter(|(_, column)| column.name == name)
                            .map(|(index, _)| index)
                            .collect::<Vec<_>>();
                        match matches.as_slice() {
                            [index] => create_projection_expression(&projections[*index], scope),
                            [] => return Err(error),
                            _ => {
                                return Err(PgError::create(
                                    SqlState::AmbiguousColumn,
                                    format!("column {name:?} is ambiguous"),
                                ));
                            }
                        }
                    }
                    Err(error) => return Err(error),
                }
            } else {
                expression.clone()
            };
            let usage = inspect_aggregate_usage(state, &resolved, scope)?;
            if usage.found {
                return Err(PgError::create(
                    SqlState::GroupingError,
                    "aggregate functions are not allowed in GROUP BY",
                ));
            }
            let data_type = infer_query_expression_type(state, &resolved, scope)?;
            Ok((resolved, data_type))
        })
        .collect()
}
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn resolve_select_lock_mode(query: &ast::Query) -> Result<Option<RowLockMode>> {
    if query.locks.len() > 1 {
        return reject_unsupported("multiple row-lock clauses are not implemented");
    }
    let Some(lock) = query.locks.first() else {
        return Ok(None);
    };
    if lock.of.is_some() || lock.nonblock.is_some() {
        return reject_unsupported("row-lock clause variant is not implemented");
    }
    Ok(Some(match lock.lock_type {
        ast::LockType::Share => RowLockMode::Share,
        ast::LockType::Update => RowLockMode::Update,
    }))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn bind_values_scope(values: &ast::Values) -> Result<BoundScope> {
    let width = values.rows.first().map(|row| row.len()).unwrap_or(0);
    if values.rows.iter().any(|row| row.len() != width) {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "VALUES lists must all be the same length",
        ));
    }
    let constants = create_constant_expression_schema();
    let columns = (0..width)
        .map(|slot| {
            let data_type = values
                .rows
                .iter()
                .map(|row| &row[slot])
                .filter(|expression| {
                    !is_null_literal(expression)
                        && extract_unknown_string_literal(expression).is_none()
                })
                .try_fold(None::<PgType>, |common, expression| {
                    let data_type =
                        infer_expression_data_type(expression, RowScope::Table(&constants))?;
                    Ok(Some(match common {
                        Some(common) => {
                            let base = coercion::resolve_common_type(common.base, data_type.base)
                                .ok_or_else(|| {
                                PgError::create(
                                    SqlState::DatatypeMismatch,
                                    "VALUES types cannot be matched",
                                )
                            })?;
                            PgType::create_with_typmod(
                                base,
                                if base == common.base
                                    && base == data_type.base
                                    && common.typmod == data_type.typmod
                                {
                                    common.typmod
                                } else {
                                    PgType::NO_TYPEMOD
                                },
                            )
                        }
                        None => data_type,
                    }))
                })?
                .unwrap_or(PgType::create(BaseType::Text));
            Ok(BoundColumn {
                name: format!("column{}", slot + 1),
                data_type,
                qualifier: String::new(),
                slot,
                merged: None,
                unqualified: true,
                wildcard: true,
                depth: 0,
                table_id: None,
                source_name: format!("column{}", slot + 1),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(BoundScope { columns })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_values_query(
    query: &ast::Query,
    values: &ast::Values,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    let scope = bind_values_scope(values)?;
    let columns = scope
        .columns
        .iter()
        .map(|column| ColumnMeta {
            name: column.name.clone(),
            type_oid: column.data_type.map_to_oid(),
            typmod: column.data_type.typmod,
        })
        .collect::<Vec<_>>();
    let constants = create_constant_expression_schema();
    let mut rows = values
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .zip(&scope.columns)
                .map(|(expression, column)| {
                    evaluate_and_coerce(
                        expression,
                        column.data_type.base,
                        CastContext::Implicit,
                        RowScope::Table(&constants),
                        &[],
                        context,
                    )
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some(order_by) = &query.order_by {
        let ast::OrderByKind::Expressions(orders) = &order_by.kind else {
            return reject_unsupported("ORDER BY ALL is not implemented");
        };
        let orders = orders
            .iter()
            .map(|order| {
                let index = if let Some(position) = extract_number_literal(&order.expr)
                    && !position.contains(['.', 'e', 'E'])
                {
                    position
                        .parse::<usize>()
                        .ok()
                        .and_then(|position| position.checked_sub(1))
                } else if let ast::Expr::Identifier(identifier) = &order.expr {
                    scope
                        .resolve_column(std::slice::from_ref(identifier))
                        .ok()
                        .map(|(slot, _)| slot)
                } else {
                    None
                }
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::InvalidColumnReference,
                        "ORDER BY position is not in select list",
                    )
                })?;
                if index >= columns.len() {
                    return Err(PgError::create(
                        SqlState::InvalidColumnReference,
                        "ORDER BY position is not in select list",
                    ));
                }
                let ascending = order.options.asc.unwrap_or(true);
                Ok((
                    index,
                    ascending,
                    order.options.nulls_first.unwrap_or(!ascending),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        rows.sort_by(|left, right| {
            orders
                .iter()
                .find_map(|(index, ascending, nulls_first)| {
                    let ordering = match (&left[*index], &right[*index]) {
                        (Value::Null, Value::Null) => Ordering::Equal,
                        (Value::Null, _) => {
                            if *nulls_first {
                                Ordering::Less
                            } else {
                                Ordering::Greater
                            }
                        }
                        (_, Value::Null) => {
                            if *nulls_first {
                                Ordering::Greater
                            } else {
                                Ordering::Less
                            }
                        }
                        (left, right) => {
                            let ordering = compare_values(left, right)
                                .expect("VALUES columns have one common type");
                            if *ascending {
                                ordering
                            } else {
                                ordering.reverse()
                            }
                        }
                    };
                    (ordering != Ordering::Equal).then_some(ordering)
                })
                .unwrap_or(Ordering::Equal)
        });
    }
    let (limit, offset) = match &query.limit_clause {
        None => (None, 0),
        Some(ast::LimitClause::LimitOffset {
            limit,
            offset,
            limit_by,
        }) if limit_by.is_empty() => (
            limit
                .as_ref()
                .map(|limit| evaluate_row_count(limit, RowCountClause::Limit, context))
                .transpose()?
                .flatten(),
            offset
                .as_ref()
                .map(|offset| evaluate_row_count(&offset.value, RowCountClause::Offset, context))
                .transpose()?
                .flatten()
                .unwrap_or(0),
        ),
        _ => {
            return reject_unsupported("LIMIT clause is not implemented");
        }
    };
    Ok(StatementResult::Query(QueryResult {
        columns,
        rows: rows
            .into_iter()
            .skip(offset)
            .take(limit.unwrap_or(usize::MAX))
            .collect(),
    }))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_set_expression(
    state: &DatabaseState,
    query: &ast::Query,
    expression: &ast::SetExpr,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<QueryResult> {
    match expression {
        ast::SetExpr::Query(query) => {
            let StatementResult::Query(result) =
                execute_query(state, query, xid, snapshot, context)?
            else {
                unreachable!("query expression produces query rows")
            };
            Ok(result)
        }
        ast::SetExpr::SetOperation {
            op,
            set_quantifier,
            left,
            right,
        } => {
            let left = execute_set_expression(state, query, left, xid, snapshot, context)?;
            let right = execute_set_expression(state, query, right, xid, snapshot, context)?;
            execute_set_operation(*op, *set_quantifier, left, right)
        }
        ast::SetExpr::Select(_) | ast::SetExpr::Values(_) => {
            let mut operand = query.clone();
            operand.with = None;
            operand.body = Box::new(expression.clone());
            operand.order_by = None;
            operand.limit_clause = None;
            operand.fetch = None;
            operand.locks.clear();
            let StatementResult::Query(result) =
                execute_query(state, &operand, xid, snapshot, context)?
            else {
                unreachable!("set operand produces query rows")
            };
            Ok(result)
        }
        _ => reject_unsupported("set-operation input is not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_set_operation(
    operator: ast::SetOperator,
    quantifier: ast::SetQuantifier,
    left: QueryResult,
    right: QueryResult,
) -> Result<QueryResult> {
    if left.columns.len() != right.columns.len() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "each set-operation query must have the same number of columns",
        ));
    }
    let columns = resolve_set_columns(&left.columns, &right.columns)?;
    let left = coerce_set_rows(left.rows, &left.columns, &columns)?;
    let right = coerce_set_rows(right.rows, &right.columns, &columns)?;
    let rows = match (operator, quantifier) {
        (ast::SetOperator::Union, ast::SetQuantifier::All) => {
            left.into_iter().chain(right).collect()
        }
        (ast::SetOperator::Union, ast::SetQuantifier::None | ast::SetQuantifier::Distinct) => {
            remove_set_duplicates(left.into_iter().chain(right).collect())?
        }
        (ast::SetOperator::Intersect, ast::SetQuantifier::All) => {
            select_set_intersection(left, right)?
        }
        (ast::SetOperator::Intersect, ast::SetQuantifier::None | ast::SetQuantifier::Distinct) => {
            select_set_intersection(remove_set_duplicates(left)?, remove_set_duplicates(right)?)?
        }
        (ast::SetOperator::Except, ast::SetQuantifier::All) => select_set_difference(left, right)?,
        (ast::SetOperator::Except, ast::SetQuantifier::None | ast::SetQuantifier::Distinct) => {
            select_set_difference(remove_set_duplicates(left)?, remove_set_duplicates(right)?)?
        }
        _ => return reject_unsupported("set-operation quantifier is not implemented"),
    };
    Ok(QueryResult { columns, rows })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_set_columns(left: &[ColumnMeta], right: &[ColumnMeta]) -> Result<Vec<ColumnMeta>> {
    if left.len() != right.len() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "each set-operation query must have the same number of columns",
        ));
    }
    left.iter()
        .zip(right)
        .map(|(left, right)| {
            let left_type = BaseType::resolve_oid(left.type_oid)
                .expect("set-operation column has a supported type OID");
            let right_type = BaseType::resolve_oid(right.type_oid)
                .expect("set-operation column has a supported type OID");
            let data_type =
                coercion::resolve_common_type(left_type, right_type).ok_or_else(|| {
                    PgError::create(
                        SqlState::DatatypeMismatch,
                        "set-operation types cannot be matched",
                    )
                })?;
            Ok(ColumnMeta {
                name: left.name.clone(),
                type_oid: data_type.map_to_oid(),
                typmod: PgType::NO_TYPEMOD,
            })
        })
        .collect()
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
fn coerce_set_rows(
    rows: Vec<Vec<Value>>,
    source: &[ColumnMeta],
    target: &[ColumnMeta],
) -> Result<Vec<Vec<Value>>> {
    rows.into_iter()
        .map(|row| {
            row.into_iter()
                .zip(source)
                .zip(target)
                .map(|((value, source), target)| {
                    coercion::coerce(
                        value,
                        BaseType::resolve_oid(source.type_oid)
                            .expect("set-operation column has a supported type OID"),
                        PgType::create(
                            BaseType::resolve_oid(target.type_oid)
                                .expect("set-operation column has a supported type OID"),
                        ),
                        CastContext::Implicit,
                    )
                })
                .collect()
        })
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn remove_set_duplicates(rows: Vec<Vec<Value>>) -> Result<Vec<Vec<Value>>> {
    let mut selected: Vec<Vec<Value>> = Vec::new();
    for row in rows {
        let mut duplicate = false;
        for existing in &selected {
            if compare_group_keys(existing, &row)? {
                duplicate = true;
                break;
            }
        }
        if !duplicate {
            selected.push(row);
        }
    }
    Ok(selected)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn select_set_intersection(
    left: Vec<Vec<Value>>,
    right: Vec<Vec<Value>>,
) -> Result<Vec<Vec<Value>>> {
    let mut consumed = vec![false; right.len()];
    let mut selected = Vec::new();
    for row in left {
        let mut match_index = None;
        for (index, candidate) in right.iter().enumerate() {
            if !consumed[index] && compare_group_keys(&row, candidate)? {
                match_index = Some(index);
                break;
            }
        }
        if let Some(index) = match_index {
            consumed[index] = true;
            selected.push(row);
        }
    }
    Ok(selected)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn select_set_difference(left: Vec<Vec<Value>>, right: Vec<Vec<Value>>) -> Result<Vec<Vec<Value>>> {
    let mut consumed = vec![false; right.len()];
    let mut selected = Vec::new();
    for row in left {
        let mut match_index = None;
        for (index, candidate) in right.iter().enumerate() {
            if !consumed[index] && compare_group_keys(&row, candidate)? {
                match_index = Some(index);
                break;
            }
        }
        if let Some(index) = match_index {
            consumed[index] = true;
        } else {
            selected.push(row);
        }
    }
    Ok(selected)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn sort_set_rows(
    rows: &mut [Vec<Value>],
    columns: &[ColumnMeta],
    query: &ast::Query,
) -> Result<()> {
    let Some(order_by) = &query.order_by else {
        return Ok(());
    };
    let ast::OrderByKind::Expressions(orders) = &order_by.kind else {
        return reject_unsupported("ORDER BY ALL is not implemented");
    };
    let orders = orders
        .iter()
        .map(|order| {
            let index = if let Some(position) = extract_number_literal(&order.expr)
                && !position.contains(['.', 'e', 'E'])
            {
                position
                    .parse::<usize>()
                    .ok()
                    .and_then(|position| position.checked_sub(1))
            } else if let ast::Expr::Identifier(identifier) = &order.expr {
                columns
                    .iter()
                    .position(|column| column.name == normalize_identifier(identifier))
            } else {
                None
            }
            .filter(|index| *index < columns.len())
            .ok_or_else(|| {
                PgError::create(
                    SqlState::InvalidColumnReference,
                    "ORDER BY position is not in select list",
                )
            })?;
            Ok((
                index,
                order.options.asc.unwrap_or(true),
                order
                    .options
                    .nulls_first
                    .unwrap_or(!order.options.asc.unwrap_or(true)),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(|left, right| {
        orders
            .iter()
            .find_map(|(index, ascending, nulls_first)| {
                let ordering = match (&left[*index], &right[*index]) {
                    (Value::Null, Value::Null) => Ordering::Equal,
                    (Value::Null, _) => {
                        if *nulls_first {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        }
                    }
                    (_, Value::Null) => {
                        if *nulls_first {
                            Ordering::Greater
                        } else {
                            Ordering::Less
                        }
                    }
                    (left, right) => {
                        let ordering = compare_values(left, right)
                            .expect("set-operation columns have one common type");
                        if *ascending {
                            ordering
                        } else {
                            ordering.reverse()
                        }
                    }
                };
                (ordering != Ordering::Equal).then_some(ordering)
            })
            .unwrap_or(Ordering::Equal)
    });
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_select_limit(
    query: &ast::Query,
    context: &StatementExecutionContext,
) -> Result<(Option<usize>, usize)> {
    match &query.limit_clause {
        None => Ok((None, 0)),
        Some(ast::LimitClause::LimitOffset {
            limit,
            offset,
            limit_by,
        }) => {
            if !limit_by.is_empty() {
                return reject_unsupported("LIMIT BY is not implemented");
            }
            let limit = limit
                .as_ref()
                .map(|limit| evaluate_row_count(limit, RowCountClause::Limit, context))
                .transpose()?
                .flatten();
            let offset = offset
                .as_ref()
                .map(|offset| evaluate_row_count(&offset.value, RowCountClause::Offset, context))
                .transpose()?
                .flatten()
                .unwrap_or(0);
            Ok((limit, offset))
        }
        Some(ast::LimitClause::OffsetCommaLimit { .. }) => {
            reject_unsupported("LIMIT clause is not implemented")
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validate_select_predicates(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
) -> Result<()> {
    if let Some(selection) = &select.selection {
        let base = infer_query_expression_type(state, selection, scope)?.base;
        if base != BaseType::Bool && !is_null_literal(selection) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    if let Some(having) = &select.having {
        let base = infer_query_expression_type(state, having, scope)?.base;
        if base != BaseType::Bool && !is_null_literal(having) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "HAVING requires a boolean expression",
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_order_specs<'a>(
    state: &DatabaseState,
    query: &'a ast::Query,
    projections: &[ProjectionSource<'_>],
    columns: &[ColumnMeta],
    scope: &BoundScope,
) -> Result<Vec<RowOrderSpec<'a>>> {
    query
        .order_by
        .as_ref()
        .map(|order_by| {
            if order_by.interpolate.is_some() {
                return reject_unsupported("ORDER BY INTERPOLATE is not implemented");
            }
            let ast::OrderByKind::Expressions(orders) = &order_by.kind else {
                return reject_unsupported("ORDER BY ALL is not implemented");
            };
            orders
                .iter()
                .map(|order| {
                    if order.with_fill.is_some() {
                        return reject_unsupported("ORDER BY WITH FILL is not implemented");
                    }
                    let key = if let Some(position) = extract_number_literal(&order.expr)
                        && !position.contains(['.', 'e', 'E'])
                    {
                        let position = position.parse::<usize>().map_err(|_| {
                            PgError::create(
                                SqlState::InvalidColumnReference,
                                "ORDER BY position is not in select list",
                            )
                        })?;
                        if position == 0 || position > projections.len() {
                            return Err(PgError::create(
                                SqlState::InvalidColumnReference,
                                "ORDER BY position is not in select list",
                            ));
                        }
                        OrderKey::Output(position - 1)
                    } else if let ast::Expr::Identifier(identifier) = &order.expr
                        && let Some(index) = columns
                            .iter()
                            .position(|column| column.name == normalize_identifier(identifier))
                    {
                        OrderKey::Output(index)
                    } else {
                        let mut output = None;
                        for (index, projection) in projections.iter().enumerate() {
                            if compare_bound_expressions(
                                &order.expr,
                                &create_projection_expression(projection, scope),
                                scope,
                            )? {
                                output = Some(index);
                                break;
                            }
                        }
                        match output {
                            Some(index) => OrderKey::Output(index),
                            None => match &order.expr {
                                ast::Expr::Identifier(identifier) => OrderKey::Input(
                                    scope.resolve_column(std::slice::from_ref(identifier))?.0,
                                ),
                                ast::Expr::CompoundIdentifier(identifiers) => {
                                    OrderKey::Input(scope.resolve_column(identifiers)?.0)
                                }
                                _ => {
                                    infer_query_expression_type(state, &order.expr, scope)?;
                                    OrderKey::Expression(&order.expr)
                                }
                            },
                        }
                    };
                    let ascending = order.options.asc.unwrap_or(true);
                    Ok(RowOrderSpec {
                        key,
                        ascending,
                        nulls_first: order.options.nulls_first.unwrap_or(!ascending),
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()
        .map(|orders| orders.unwrap_or_default())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_order_expression(
    order: &RowOrderSpec<'_>,
    projections: &[ProjectionSource<'_>],
    scope: &BoundScope,
) -> ast::Expr {
    match order.key {
        OrderKey::Output(index) => create_projection_expression(&projections[index], scope),
        OrderKey::Input(slot) => {
            create_projection_expression(&ProjectionSource::Column(slot), scope)
        }
        OrderKey::Expression(expression) => expression.clone(),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_distinct_plan<'a>(
    state: &DatabaseState,
    select: &'a ast::Select,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    scope: &BoundScope,
) -> Result<DistinctPlan<'a>> {
    let Some(distinct) = &select.distinct else {
        return Ok(DistinctPlan::None);
    };
    match distinct {
        ast::Distinct::All => Ok(DistinctPlan::None),
        ast::Distinct::Distinct => {
            for order in order_specs {
                if matches!(order.key, OrderKey::Output(_)) {
                    continue;
                }
                let order_expression = create_order_expression(order, projections, scope);
                let mut selected = false;
                for projection in projections {
                    if compare_bound_expressions(
                        &order_expression,
                        &create_projection_expression(projection, scope),
                        scope,
                    )? {
                        selected = true;
                        break;
                    }
                }
                if !selected {
                    return Err(PgError::create(
                        SqlState::InvalidColumnReference,
                        "for SELECT DISTINCT, ORDER BY expressions must appear in select list",
                    ));
                }
            }
            Ok(DistinctPlan::Rows)
        }
        ast::Distinct::On(expressions) => {
            if expressions.is_empty() {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "DISTINCT ON requires at least one expression",
                ));
            }
            for expression in expressions {
                infer_query_expression_type(state, expression, scope)?;
            }
            let mut matched = vec![false; expressions.len()];
            for order in order_specs {
                let order_expression = create_order_expression(order, projections, scope);
                let mut found = None;
                for (index, expression) in expressions.iter().enumerate() {
                    if !matched[index]
                        && compare_bound_expressions(&order_expression, expression, scope)?
                    {
                        found = Some(index);
                        break;
                    }
                }
                match found {
                    Some(index) => matched[index] = true,
                    None if matched.iter().all(|matched| *matched) => break,
                    None => {
                        return Err(PgError::create(
                            SqlState::InvalidColumnReference,
                            "SELECT DISTINCT ON expressions must match initial ORDER BY expressions",
                        ));
                    }
                }
            }
            let mut keys = Vec::with_capacity(expressions.len());
            for expression in expressions {
                let mut key = None;
                for (index, projection) in projections.iter().enumerate() {
                    if compare_bound_expressions(
                        expression,
                        &create_projection_expression(projection, scope),
                        scope,
                    )? {
                        key = Some(DistinctKey::Output(index));
                        break;
                    }
                }
                if key.is_none() {
                    for (index, order) in order_specs.iter().enumerate() {
                        if compare_bound_expressions(
                            expression,
                            &create_order_expression(order, projections, scope),
                            scope,
                        )? {
                            key = Some(DistinctKey::Order(index));
                            break;
                        }
                    }
                }
                keys.push(key.unwrap_or(DistinctKey::Expression(expression)));
            }
            Ok(DistinctPlan::On { expressions, keys })
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn extend_grouped_columns_with_primary_keys(
    state: &DatabaseState,
    scope: &BoundScope,
    grouped_columns: &mut Vec<(usize, PgType)>,
) {
    let mut checked_relations = Vec::new();
    for column in scope.columns.iter().filter(|column| column.depth == 0) {
        let Some(table_id) = column.table_id else {
            continue;
        };
        let relation = (table_id, column.qualifier.clone());
        if checked_relations.contains(&relation) {
            continue;
        }
        checked_relations.push(relation.clone());
        let table = state
            .catalog
            .iterate_tables()
            .find(|table| table.id == table_id)
            .expect("bound base table remains in the catalog");
        let Some(primary_key) = table
            .constraints
            .iter()
            .find_map(|constraint| match constraint {
                crate::catalog::Constraint::PrimaryKey(columns) => Some(columns),
                _ => None,
            })
        else {
            continue;
        };
        let relation_columns = scope
            .columns
            .iter()
            .filter(|column| {
                column.depth == 0
                    && column.table_id == Some(table_id)
                    && column.qualifier == relation.1
            })
            .collect::<Vec<_>>();
        let primary_key_is_grouped = primary_key.iter().all(|name| {
            let matches = relation_columns
                .iter()
                .filter(|column| column.source_name == *name)
                .collect::<Vec<_>>();
            matches.len() == 1
                && grouped_columns
                    .iter()
                    .any(|(slot, _)| *slot == matches[0].slot)
        });
        if primary_key_is_grouped {
            for column in relation_columns {
                if !grouped_columns.iter().any(|(slot, _)| *slot == column.slot) {
                    grouped_columns.push((column.slot, column.data_type));
                }
            }
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_grouping_plan(
    state: &DatabaseState,
    select: &ast::Select,
    group_by: &[ast::Expr],
    projections: &[ProjectionSource<'_>],
    columns: &[ColumnMeta],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    scope: &BoundScope,
) -> Result<GroupingPlan> {
    let expressions = resolve_grouping_expressions(state, group_by, projections, columns, scope)?;
    let mut grouped_columns = expressions
        .iter()
        .filter_map(|(expression, _)| match expression {
            ast::Expr::Identifier(identifier) => {
                scope.resolve_column(std::slice::from_ref(identifier)).ok()
            }
            ast::Expr::CompoundIdentifier(identifiers) => scope.resolve_column(identifiers).ok(),
            _ => None,
        })
        .collect::<Vec<_>>();
    extend_grouped_columns_with_primary_keys(state, scope, &mut grouped_columns);

    let mut aggregate_query = false;
    for item in &select.projection {
        if let ast::SelectItem::UnnamedExpr(expression)
        | ast::SelectItem::ExprWithAlias {
            expr: expression, ..
        } = item
        {
            aggregate_query |= inspect_aggregate_usage(state, expression, scope)?.found;
        }
    }
    if let Some(having) = &select.having {
        aggregate_query |= inspect_aggregate_usage(state, having, scope)?.found;
    }
    for order in order_specs {
        if let OrderKey::Expression(expression) = order.key {
            aggregate_query |= inspect_aggregate_usage(state, expression, scope)?.found;
        }
    }
    if let DistinctPlan::On {
        expressions: distinct_expressions,
        ..
    } = distinct
    {
        for expression in *distinct_expressions {
            aggregate_query |= inspect_aggregate_usage(state, expression, scope)?.found;
        }
    }
    let enabled = aggregate_query || !expressions.is_empty() || select.having.is_some();
    if enabled {
        for projection in projections {
            validate_grouped_expression(
                state,
                &create_projection_expression(projection, scope),
                scope,
                &expressions,
                &grouped_columns,
            )?;
        }
        if let Some(having) = &select.having {
            validate_grouped_expression(state, having, scope, &expressions, &grouped_columns)?;
        }
        for order in order_specs {
            if let OrderKey::Expression(expression) = order.key {
                validate_grouped_expression(
                    state,
                    expression,
                    scope,
                    &expressions,
                    &grouped_columns,
                )?;
            }
        }
        if let DistinctPlan::On {
            expressions: distinct_expressions,
            ..
        } = distinct
        {
            for expression in *distinct_expressions {
                validate_grouped_expression(
                    state,
                    expression,
                    scope,
                    &expressions,
                    &grouped_columns,
                )?;
            }
        }
    }
    Ok(GroupingPlan {
        expressions,
        enabled,
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_where_clause(
    state: &DatabaseState,
    selection: Option<&ast::Expr>,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<bool> {
    let Some(selection) = selection else {
        return Ok(true);
    };
    Ok(
        match evaluate_query_expression(state, selection, scope, row, xid, snapshot, context)? {
            Value::Bool(value) => value,
            Value::Null => false,
            _ => unreachable!("WHERE expression was type-checked"),
        },
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_select_expression(
    state: &DatabaseState,
    expression: &ast::Expr,
    scope: &BoundScope,
    row: &[Value],
    aggregate_rows: Option<&[Vec<Value>]>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Value> {
    let materialized;
    let expression = if let Some(rows) = aggregate_rows {
        materialized = materialize_aggregate_expression(
            state, expression, scope, rows, xid, snapshot, context,
        )?;
        &materialized
    } else {
        expression
    };
    evaluate_query_expression(state, expression, scope, row, xid, snapshot, context)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_projection_value(
    state: &DatabaseState,
    projection: &ProjectionSource<'_>,
    scope: &BoundScope,
    row: &[Value],
    aggregate_rows: Option<&[Vec<Value>]>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Value> {
    match projection {
        ProjectionSource::Column(index) => Ok(row[*index].clone()),
        ProjectionSource::Merged(left, right, data_type) => {
            let value = if row[*left].is_null() {
                row[*right].clone()
            } else {
                row[*left].clone()
            };
            if value.is_null() {
                Ok(value)
            } else {
                coercion::coerce(
                    value.clone(),
                    value
                        .get_base_type()
                        .expect("non-null value has a base type"),
                    *data_type,
                    CastContext::Implicit,
                )
            }
        }
        ProjectionSource::Expression(expression) => evaluate_select_expression(
            state,
            expression,
            scope,
            row,
            aggregate_rows,
            xid,
            snapshot,
            context,
        ),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_projection_values(
    state: &DatabaseState,
    projections: &[ProjectionSource<'_>],
    scope: &BoundScope,
    row: &[Value],
    aggregate_rows: Option<&[Vec<Value>]>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<Value>> {
    projections
        .iter()
        .map(|projection| {
            evaluate_projection_value(
                state,
                projection,
                scope,
                row,
                aggregate_rows,
                xid,
                snapshot,
                context,
            )
        })
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_order_keys(
    state: &DatabaseState,
    order_specs: &[RowOrderSpec<'_>],
    values: &[Value],
    scope: &BoundScope,
    row: &[Value],
    aggregate_rows: Option<&[Vec<Value>]>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<Value>> {
    order_specs
        .iter()
        .map(|order| match order.key {
            OrderKey::Output(index) => Ok(values[index].clone()),
            OrderKey::Input(slot) => Ok(row[slot].clone()),
            OrderKey::Expression(expression) => evaluate_select_expression(
                state,
                expression,
                scope,
                row,
                aggregate_rows,
                xid,
                snapshot,
                context,
            ),
        })
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_distinct_keys(
    state: &DatabaseState,
    distinct: &DistinctPlan<'_>,
    values: &[Value],
    order_keys: &[Value],
    scope: &BoundScope,
    row: &[Value],
    aggregate_rows: Option<&[Vec<Value>]>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<Value>> {
    let DistinctPlan::On { keys, .. } = distinct else {
        return Ok(Vec::new());
    };
    keys.iter()
        .map(|key| match key {
            DistinctKey::Output(index) => Ok(values[*index].clone()),
            DistinctKey::Order(index) => Ok(order_keys[*index].clone()),
            DistinctKey::Expression(expression) => evaluate_select_expression(
                state,
                expression,
                scope,
                row,
                aggregate_rows,
                xid,
                snapshot,
                context,
            ),
        })
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_plain_select_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    top_k: Option<usize>,
) -> Result<Vec<OrderedRow>> {
    if let Some(rows) = execute_correlated_exists_rows(
        state,
        select,
        scope,
        projections,
        order_specs,
        distinct,
        xid,
        snapshot,
        context,
    ) {
        return rows;
    }
    if let Some(rows) = execute_any_membership_rows(
        state,
        select,
        scope,
        projections,
        order_specs,
        distinct,
        xid,
        snapshot,
        context,
    ) {
        return rows;
    }
    let mut rows = Vec::new();
    let remaining_selection = if selection_is_fully_pushed(select, scope) {
        None
    } else {
        select.selection.as_ref()
    };
    visit_query_source_rows(
        state,
        select,
        scope,
        xid,
        snapshot,
        context,
        select.selection.as_ref(),
        &mut |row| {
            if !evaluate_where_clause(
                state,
                remaining_selection,
                scope,
                row,
                xid,
                snapshot,
                context,
            )? {
                return Ok(());
            }
            let values = evaluate_projection_values(
                state,
                projections,
                scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            let keys = evaluate_order_keys(
                state,
                order_specs,
                &values,
                scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            let distinct_keys = evaluate_distinct_keys(
                state, distinct, &values, &keys, scope, row, None, xid, snapshot, context,
            )?;
            retain_top_ordered_row(
                &mut rows,
                OrderedRow {
                    values,
                    keys,
                    distinct_keys,
                },
                top_k,
                order_specs,
            );
            Ok(())
        },
    )?;
    Ok(rows)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_correlated_exists_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Option<Result<Vec<OrderedRow>>> {
    let ast::Expr::Exists {
        subquery,
        negated: false,
    } = select.selection.as_ref()?
    else {
        return None;
    };
    let ast::SetExpr::Select(inner_select) = subquery.body.as_ref() else {
        return None;
    };
    let [
        ast::TableWithJoins {
            relation: ast::TableFactor::Table {
                name, args: None, ..
            },
            joins,
        },
    ] = inner_select.from.as_slice()
    else {
        return None;
    };
    let ast::GroupByExpr::Expressions(group_by, modifiers) = &inner_select.group_by else {
        return None;
    };
    let Some(ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::Eq,
        right,
    }) = inner_select.selection.as_ref()
    else {
        return None;
    };
    if !joins.is_empty()
        || inner_select.distinct.is_some()
        || !group_by.is_empty()
        || !modifiers.is_empty()
        || inner_select.having.is_some()
        || inner_select.into.is_some()
        || subquery.with.is_some()
        || subquery.order_by.is_some()
        || subquery.limit_clause.is_some()
        || subquery.fetch.is_some()
    {
        return None;
    }
    let inner_scope = match bind_select_scope(state, inner_select) {
        Ok(scope) => scope,
        Err(error) => return Some(Err(error)),
    };
    let (inner_slot, inner_type, outer_slot, outer_type) = [
        (left.as_ref(), right.as_ref()),
        (right.as_ref(), left.as_ref()),
    ]
    .into_iter()
    .find_map(|(inner, outer)| {
        let (inner_slot, inner_type) = resolve_hash_expression_slot(inner, &inner_scope)?;
        let (outer_slot, outer_type) = resolve_hash_expression_slot(outer, scope)?;
        resolve_hash_expression_slot(outer, &inner_scope)
            .is_none()
            .then_some((inner_slot, inner_type, outer_slot, outer_type))
    })?;
    if inner_type != outer_type {
        return None;
    }
    let table_name = match normalize_unqualified_object_name(name) {
        Ok(name) => name,
        Err(error) => return Some(Err(error)),
    };
    let schema = match state.catalog.require_table(&table_name) {
        Ok(schema) => schema,
        Err(error) => return Some(Err(error)),
    };
    let mut matches = std::collections::HashSet::new();
    for (_, chain) in state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .iterate_version_chains()
    {
        if let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions)
            && let Some(key) = create_hash_join_key(&version.row[inner_slot])
        {
            matches.insert(key);
        }
    }
    let mut rows = Vec::new();
    let result = visit_query_source_rows(
        state,
        select,
        scope,
        xid,
        snapshot,
        context,
        None,
        &mut |row| {
            let Some(key) = create_hash_join_key(&row[outer_slot]) else {
                return Ok(());
            };
            if !matches.contains(&key) {
                return Ok(());
            }
            let values = evaluate_projection_values(
                state,
                projections,
                scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            let keys = evaluate_order_keys(
                state,
                order_specs,
                &values,
                scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            let distinct_keys = evaluate_distinct_keys(
                state, distinct, &values, &keys, scope, row, None, xid, snapshot, context,
            )?;
            rows.push(OrderedRow {
                values,
                keys,
                distinct_keys,
            });
            Ok(())
        },
    );
    Some(result.map(|()| rows))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_any_membership_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Option<Result<Vec<OrderedRow>>> {
    let ast::Expr::AnyOp {
        left,
        compare_op: ast::BinaryOperator::Eq,
        right,
        ..
    } = select.selection.as_ref()?
    else {
        return None;
    };
    let ast::Expr::Tuple(candidates) = right.as_ref() else {
        return None;
    };
    if !candidates.iter().all(|candidate| {
        matches!(candidate, ast::Expr::Cast { expr, .. } if matches!(expr.as_ref(), ast::Expr::Value(_)))
    }) {
        return None;
    }
    let left_type = match infer_query_expression_type(state, left, scope) {
        Ok(data_type) => data_type,
        Err(error) => return Some(Err(error)),
    };
    if !matches!(
        left_type.base,
        BaseType::Bool
            | BaseType::Int2
            | BaseType::Int4
            | BaseType::Int8
            | BaseType::Text
            | BaseType::Varchar
            | BaseType::Bpchar
            | BaseType::Bytea
            | BaseType::Uuid
    ) {
        return None;
    }
    let empty_row = vec![Value::Null; scope.columns.len()];
    let mut matches = std::collections::HashSet::new();
    for candidate in candidates {
        let value = match evaluate_and_coerce(
            candidate,
            left_type.base,
            CastContext::Implicit,
            RowScope::Bound(scope),
            &empty_row,
            context,
        ) {
            Ok(value) => value,
            Err(error) => return Some(Err(error)),
        };
        if let Some(key) = create_hash_join_key(&value) {
            matches.insert(key);
        }
    }
    let mut rows = Vec::new();
    let result = visit_query_source_rows(
        state,
        select,
        scope,
        xid,
        snapshot,
        context,
        None,
        &mut |row| {
            let value = evaluate_and_coerce(
                left,
                left_type.base,
                CastContext::Implicit,
                RowScope::Bound(scope),
                row,
                context,
            )?;
            let Some(key) = create_hash_join_key(&value) else {
                return Ok(());
            };
            if !matches.contains(&key) {
                return Ok(());
            }
            let values = evaluate_projection_values(
                state,
                projections,
                scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            let keys = evaluate_order_keys(
                state,
                order_specs,
                &values,
                scope,
                row,
                None,
                xid,
                snapshot,
                context,
            )?;
            let distinct_keys = evaluate_distinct_keys(
                state, distinct, &values, &keys, scope, row, None, xid, snapshot, context,
            )?;
            rows.push(OrderedRow {
                values,
                keys,
                distinct_keys,
            });
            Ok(())
        },
    );
    Some(result.map(|()| rows))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_grouped_select_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    grouped_expressions: &[(ast::Expr, PgType)],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<OrderedRow>> {
    let mut groups = if grouped_expressions.is_empty() {
        vec![(Vec::new(), Vec::new())]
    } else {
        Vec::new()
    };
    visit_query_source_rows(
        state,
        select,
        scope,
        xid,
        snapshot,
        context,
        select.selection.as_ref(),
        &mut |row| {
            if !evaluate_where_clause(
                state,
                select.selection.as_ref(),
                scope,
                row,
                xid,
                snapshot,
                context,
            )? {
                return Ok(());
            }
            let key = grouped_expressions
                .iter()
                .map(|(expression, _)| {
                    evaluate_query_expression(state, expression, scope, row, xid, snapshot, context)
                })
                .collect::<Result<Vec<_>>>()?;
            let mut matching = None;
            for (index, (group_key, _)) in groups.iter().enumerate() {
                if compare_group_keys(group_key, &key)? {
                    matching = Some(index);
                    break;
                }
            }
            match matching {
                Some(index) => groups[index].1.push(row.to_vec()),
                None => groups.push((key, vec![row.to_vec()])),
            }
            Ok(())
        },
    )?;

    let mut rows = Vec::new();
    for (_, group_rows) in groups {
        let empty_row = vec![Value::Null; scope.columns.len()];
        let row = group_rows.first().unwrap_or(&empty_row);
        if let Some(having) = &select.having {
            let expression = materialize_aggregate_expression(
                state,
                having,
                scope,
                &group_rows,
                xid,
                snapshot,
                context,
            )?;
            match evaluate_query_expression(state, &expression, scope, row, xid, snapshot, context)?
            {
                Value::Bool(true) => {}
                Value::Bool(false) | Value::Null => continue,
                _ => unreachable!("HAVING expression was type-checked"),
            }
        }
        let values = evaluate_projection_values(
            state,
            projections,
            scope,
            row,
            Some(&group_rows),
            xid,
            snapshot,
            context,
        )?;
        let keys = evaluate_order_keys(
            state,
            order_specs,
            &values,
            scope,
            row,
            Some(&group_rows),
            xid,
            snapshot,
            context,
        )?;
        let distinct_keys = evaluate_distinct_keys(
            state,
            distinct,
            &values,
            &keys,
            scope,
            row,
            Some(&group_rows),
            xid,
            snapshot,
            context,
        )?;
        rows.push(OrderedRow {
            values,
            keys,
            distinct_keys,
        });
    }
    Ok(rows)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn sort_ordered_rows(rows: &mut [OrderedRow], order_specs: &[RowOrderSpec<'_>]) {
    if !order_specs.is_empty() {
        rows.sort_by(|left, right| compare_ordered_rows(left, right, order_specs));
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn compare_ordered_rows(
    left: &OrderedRow,
    right: &OrderedRow,
    order_specs: &[RowOrderSpec<'_>],
) -> Ordering {
    order_specs
        .iter()
        .zip(left.keys.iter().zip(&right.keys))
        .find_map(|(spec, (left, right))| {
            let ordering = match (left, right) {
                (Value::Null, Value::Null) => Ordering::Equal,
                (Value::Null, _) => {
                    if spec.nulls_first {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (_, Value::Null) => {
                    if spec.nulls_first {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                _ => {
                    let ordering =
                        compare_values(left, right).expect("ORDER BY expression type was checked");
                    if spec.ascending {
                        ordering
                    } else {
                        ordering.reverse()
                    }
                }
            };
            (ordering != Ordering::Equal).then_some(ordering)
        })
        .unwrap_or(Ordering::Equal)
}

fn retain_top_ordered_row(
    rows: &mut Vec<OrderedRow>,
    row: OrderedRow,
    top_k: Option<usize>,
    order_specs: &[RowOrderSpec<'_>],
) {
    let Some(top_k) = top_k else {
        rows.push(row);
        return;
    };
    if top_k == 0 {
        return;
    }
    if rows.len() < top_k {
        rows.push(row);
        let mut child = rows.len() - 1;
        while child > 0 {
            let parent = (child - 1) / 2;
            if compare_ordered_rows(&rows[parent], &rows[child], order_specs) != Ordering::Less {
                break;
            }
            rows.swap(parent, child);
            child = parent;
        }
        return;
    }
    if compare_ordered_rows(&row, &rows[0], order_specs) != Ordering::Less {
        return;
    }
    rows[0] = row;
    let mut parent = 0;
    loop {
        let left = parent * 2 + 1;
        if left >= rows.len() {
            break;
        }
        let right = left + 1;
        let worse_child = if right < rows.len()
            && compare_ordered_rows(&rows[left], &rows[right], order_specs) == Ordering::Less
        {
            right
        } else {
            left
        };
        if compare_ordered_rows(&rows[parent], &rows[worse_child], order_specs) != Ordering::Less {
            break;
        }
        rows.swap(parent, worse_child);
        parent = worse_child;
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn remove_duplicate_rows(
    rows: Vec<OrderedRow>,
    distinct: &DistinctPlan<'_>,
) -> Result<Vec<OrderedRow>> {
    let mut selected: Vec<OrderedRow> = Vec::new();
    for row in rows {
        let key = match distinct {
            DistinctPlan::None => {
                selected.push(row);
                continue;
            }
            DistinctPlan::Rows => &row.values,
            DistinctPlan::On { .. } => &row.distinct_keys,
        };
        let mut duplicate = false;
        for existing in &selected {
            let existing_key = match distinct {
                DistinctPlan::Rows => &existing.values,
                DistinctPlan::On { .. } => &existing.distinct_keys,
                DistinctPlan::None => unreachable!("non-distinct rows returned before comparison"),
            };
            if compare_group_keys(existing_key, key)? {
                duplicate = true;
                break;
            }
        }
        if !duplicate {
            selected.push(row);
        }
    }
    Ok(selected)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn finalize_select_rows(
    mut rows: Vec<OrderedRow>,
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<Vec<Value>>> {
    if matches!(distinct, DistinctPlan::Rows) {
        rows = remove_duplicate_rows(rows, distinct)?;
    }
    if !order_specs.is_empty()
        && matches!(distinct, DistinctPlan::None)
        && let Some(limit) = limit
    {
        let required = offset.saturating_add(limit);
        if required < rows.len() {
            rows.select_nth_unstable_by(required, |left, right| {
                compare_ordered_rows(left, right, order_specs)
            });
            rows.truncate(required);
        }
    }
    sort_ordered_rows(&mut rows, order_specs);
    if matches!(distinct, DistinctPlan::On { .. }) {
        rows = remove_duplicate_rows(rows, distinct)?;
    }
    Ok(rows
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .map(|row| row.values)
        .collect())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_query(
    state: &DatabaseState,
    query: &ast::Query,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    if query.with.is_some() {
        let query = materialize_query_ctes(state, query, xid, snapshot, context)?;
        return execute_query(state, &query, xid, snapshot, context);
    }
    if query.fetch.is_some() {
        return reject_unsupported("query clause is not implemented");
    }
    let lock_mode = resolve_select_lock_mode(query)?;
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        if let ast::SetExpr::Values(values) = query.body.as_ref() {
            return execute_values_query(query, values, context);
        }
        if lock_mode.is_some() {
            return reject_unsupported("FOR UPDATE is not allowed with set operations");
        }
        let mut result = execute_set_expression(state, query, &query.body, xid, snapshot, context)?;
        sort_set_rows(&mut result.rows, &result.columns, query)?;
        let (limit, offset) = resolve_select_limit(query, context)?;
        result.rows = result
            .rows
            .into_iter()
            .skip(offset)
            .take(limit.unwrap_or(usize::MAX))
            .collect();
        return Ok(StatementResult::Query(result));
    };
    let ast::GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
        return reject_unsupported("GROUP BY is not implemented");
    };
    if select.into.is_some() || !modifiers.is_empty() {
        return reject_unsupported("SELECT feature is not implemented");
    }

    let scope = bind_select_scope(state, select)?;
    if let Some(selection) = &select.selection
        && inspect_aggregate_usage(state, selection, &scope)?.found
    {
        return Err(PgError::create(
            SqlState::GroupingError,
            "aggregate functions are not allowed in WHERE",
        ));
    }
    let (limit, offset) = resolve_select_limit(query, context)?;
    validate_select_predicates(state, select, &scope)?;

    let (projections, columns) = build_projection_plan(state, &select.projection, &scope)?;
    let order_specs = resolve_order_specs(state, query, &projections, &columns, &scope)?;
    let distinct = resolve_distinct_plan(state, select, &projections, &order_specs, &scope)?;
    let grouping = resolve_grouping_plan(
        state,
        select,
        group_by,
        &projections,
        &columns,
        &order_specs,
        &distinct,
        &scope,
    )?;
    if grouping.enabled && lock_mode.is_some() {
        return reject_unsupported("FOR UPDATE is not allowed with aggregate functions");
    }
    if !matches!(distinct, DistinctPlan::None) && lock_mode.is_some() {
        return reject_unsupported("FOR UPDATE is not allowed with DISTINCT clause");
    }

    let rows = if grouping.enabled {
        execute_grouped_select_rows(
            state,
            select,
            &scope,
            &projections,
            &order_specs,
            &distinct,
            &grouping.expressions,
            xid,
            snapshot,
            context,
        )?
    } else {
        let top_k = if !order_specs.is_empty() && matches!(distinct, DistinctPlan::None) {
            limit.map(|limit| offset.saturating_add(limit))
        } else {
            None
        };
        execute_plain_select_rows(
            state,
            select,
            &scope,
            &projections,
            &order_specs,
            &distinct,
            xid,
            snapshot,
            context,
            top_k,
        )?
    };
    let rows = finalize_select_rows(rows, &order_specs, &distinct, limit, offset)?;
    Ok(StatementResult::Query(QueryResult { columns, rows }))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn build_projection_plan<'a>(
    state: &DatabaseState,
    projection: &'a [ast::SelectItem],
    scope: &BoundScope,
) -> Result<(Vec<ProjectionSource<'a>>, Vec<ColumnMeta>)> {
    let mut projections = Vec::new();
    let mut columns = Vec::new();
    for item in projection {
        match item {
            ast::SelectItem::Wildcard(_) => {
                for column in &scope.columns {
                    if column.wildcard {
                        projections.push(match column.merged {
                            Some((left, right)) => {
                                ProjectionSource::Merged(left, right, column.data_type)
                            }
                            None => ProjectionSource::Column(column.slot),
                        });
                        columns.push(ColumnMeta {
                            name: column.name.clone(),
                            type_oid: column.data_type.map_to_oid(),
                            typmod: column.data_type.typmod,
                        });
                    }
                }
            }
            ast::SelectItem::QualifiedWildcard(
                ast::SelectItemQualifiedWildcardKind::ObjectName(object_name),
                _,
            ) => {
                let qualifier = normalize_unqualified_object_name(object_name)?;
                let matching = scope
                    .columns
                    .iter()
                    .filter(|column| column.qualifier == qualifier && column.wildcard)
                    .collect::<Vec<_>>();
                if matching.is_empty()
                    && !scope
                        .columns
                        .iter()
                        .any(|column| column.qualifier == qualifier)
                {
                    return Err(PgError::create(
                        SqlState::UndefinedTable,
                        format!("missing FROM-clause entry for table {qualifier:?}"),
                    ));
                }
                for column in matching {
                    projections.push(ProjectionSource::Column(column.slot));
                    columns.push(ColumnMeta {
                        name: column.name.clone(),
                        type_oid: column.data_type.map_to_oid(),
                        typmod: column.data_type.typmod,
                    });
                }
            }
            ast::SelectItem::UnnamedExpr(expression @ ast::Expr::Identifier(column)) => {
                let (_, data_type) = scope.resolve_column(std::slice::from_ref(column))?;
                projections.push(ProjectionSource::Expression(expression));
                columns.push(ColumnMeta {
                    name: column.value.clone(),
                    type_oid: data_type.map_to_oid(),
                    typmod: data_type.typmod,
                });
            }
            ast::SelectItem::UnnamedExpr(
                expression @ ast::Expr::CompoundIdentifier(identifiers),
            ) => {
                let (_, data_type) = scope.resolve_column(identifiers)?;
                projections.push(ProjectionSource::Expression(expression));
                columns.push(ColumnMeta {
                    name: identifiers
                        .last()
                        .expect("compound identifier is non-empty")
                        .value
                        .clone(),
                    type_oid: data_type.map_to_oid(),
                    typmod: data_type.typmod,
                });
            }
            ast::SelectItem::UnnamedExpr(expr) => {
                let data_type = infer_query_expression_type(state, expr, scope)?;
                projections.push(ProjectionSource::Expression(expr));
                columns.push(ColumnMeta {
                    name: match expr {
                        ast::Expr::Function(function) if is_aggregate_function(function) => {
                            normalize_unqualified_object_name(&function.name)?
                        }
                        _ => "?column?".into(),
                    },
                    type_oid: data_type.map_to_oid(),
                    typmod: data_type.typmod,
                });
            }
            ast::SelectItem::ExprWithAlias { expr, alias } => {
                let resolved = match expr {
                    ast::Expr::Identifier(column) => {
                        Some(scope.resolve_column(std::slice::from_ref(column))?)
                    }
                    ast::Expr::CompoundIdentifier(identifiers) => {
                        Some(scope.resolve_column(identifiers)?)
                    }
                    _ => None,
                };
                let (projection, data_type, typmod) = match resolved {
                    Some((_, data_type)) => (
                        ProjectionSource::Expression(expr),
                        data_type,
                        data_type.typmod,
                    ),
                    None => {
                        let data_type = infer_query_expression_type(state, expr, scope)?;
                        (
                            ProjectionSource::Expression(expr),
                            data_type,
                            data_type.typmod,
                        )
                    }
                };
                projections.push(projection);
                columns.push(ColumnMeta {
                    name: normalize_identifier(alias),
                    type_oid: data_type.map_to_oid(),
                    typmod,
                });
            }
            _ => {
                return reject_unsupported("SELECT projection is not implemented");
            }
        }
    }
    Ok((projections, columns))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn build_mutation_projection_plan<'a>(
    state: &DatabaseState,
    projection: &'a [ast::SelectItem],
    scope: &BoundScope,
    target_columns: usize,
) -> Result<(Vec<ProjectionSource<'a>>, Vec<ColumnMeta>)> {
    let mut target_wildcard_scope = scope.clone();
    for column in &mut target_wildcard_scope.columns[target_columns..] {
        column.wildcard = false;
    }
    let mut projections = Vec::new();
    let mut columns = Vec::new();
    for item in projection {
        let item_scope = if matches!(item, ast::SelectItem::Wildcard(_)) {
            &target_wildcard_scope
        } else {
            scope
        };
        let (mut item_projections, mut item_columns) =
            build_projection_plan(state, std::slice::from_ref(item), item_scope)?;
        projections.append(&mut item_projections);
        columns.append(&mut item_columns);
    }
    Ok((projections, columns))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn infer_query_expression_type(
    state: &DatabaseState,
    expr: &ast::Expr,
    scope: &BoundScope,
) -> Result<PgType> {
    super::scope::infer_expression_data_type(&state.catalog, expr, scope)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materialize_source_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
) -> Result<Vec<Vec<Value>>> {
    materialize_from_rows(
        state,
        &select.from,
        scope,
        0,
        xid,
        snapshot,
        context,
        selection,
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn materialize_from_rows(
    state: &DatabaseState,
    from: &[ast::TableWithJoins],
    scope: &BoundScope,
    start_slot: usize,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
) -> Result<Vec<Vec<Value>>> {
    if from.is_empty() {
        return Ok(vec![Vec::new()]);
    }
    let mut next_slot = start_slot;
    let mut rows = vec![vec![Value::Null; scope.columns.len()]];
    for table in from {
        let source = materialize_table_with_joins_rows(
            state,
            table,
            scope,
            xid,
            snapshot,
            context,
            selection,
            &mut next_slot,
        )?;
        rows = rows
            .into_iter()
            .flat_map(|left| {
                source.iter().map(move |right| {
                    left.iter()
                        .zip(right)
                        .map(|(left, right)| {
                            if left.is_null() {
                                right.clone()
                            } else {
                                left.clone()
                            }
                        })
                        .collect()
                })
            })
            .collect();
    }
    Ok(rows)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn visit_query_source_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    if let [table] = select.from.as_slice()
        && can_stream_join(table)
    {
        return visit_streamed_join_rows(
            state, table, scope, xid, snapshot, context, selection, visit,
        );
    }
    for row in materialize_source_rows(state, select, scope, xid, snapshot, context, selection)? {
        visit(&row)?;
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn can_stream_join(table: &ast::TableWithJoins) -> bool {
    matches!(table.relation, ast::TableFactor::Table { .. })
        && table.joins.iter().all(|join| {
            matches!(join.relation, ast::TableFactor::Table { .. })
                && matches!(
                    join.join_operator,
                    ast::JoinOperator::Join(_)
                        | ast::JoinOperator::Inner(_)
                        | ast::JoinOperator::CrossJoin(_)
                        | ast::JoinOperator::Left(_)
                        | ast::JoinOperator::LeftOuter(_)
                )
        })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn visit_streamed_join_rows(
    state: &DatabaseState,
    table: &ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let mut starts = Vec::with_capacity(table.joins.len() + 1);
    let mut next_slot = 0;
    for factor in
        std::iter::once(&table.relation).chain(table.joins.iter().map(|join| &join.relation))
    {
        let ast::TableFactor::Table {
            name: table_name, ..
        } = factor
        else {
            unreachable!("streamable sources are tables");
        };
        starts.push(next_slot);
        next_slot += state
            .catalog
            .require_table(&normalize_unqualified_object_name(table_name)?)?
            .columns
            .len();
    }
    let hash_slots = table
        .joins
        .iter()
        .enumerate()
        .map(|(index, join)| {
            resolve_hash_join_slots(&join.join_operator, scope, starts[0], starts[index + 1])
        })
        .collect::<Option<Vec<_>>>();
    if let Some(hash_slots) = hash_slots
        && !hash_slots.is_empty()
    {
        return visit_hash_join_chain_rows(
            state,
            table,
            scope,
            xid,
            snapshot,
            context,
            selection,
            &starts,
            &hash_slots,
            visit,
        );
    }
    let mut right_sources = Vec::with_capacity(table.joins.len());
    for (index, join) in table.joins.iter().enumerate() {
        let mut rows = Vec::new();
        visit_table_factor_rows(
            state,
            &join.relation,
            scope,
            xid,
            snapshot,
            context,
            selection,
            starts[index + 1],
            &mut |row| {
                rows.push(row.to_vec());
                Ok(())
            },
        )?;
        right_sources.push(rows);
    }
    visit_table_factor_rows(
        state,
        &table.relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        starts[0],
        &mut |row| {
            visit_nested_loop_join_rows(
                state,
                table,
                scope,
                xid,
                snapshot,
                context,
                &starts,
                &right_sources,
                0,
                row,
                visit,
            )
        },
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_hash_join_slots(
    operator: &ast::JoinOperator,
    scope: &BoundScope,
    left_start: usize,
    right_start: usize,
) -> Option<(usize, usize, bool)> {
    let (ast::JoinOperator::Join(ast::JoinConstraint::On(expression))
    | ast::JoinOperator::Inner(ast::JoinConstraint::On(expression))
    | ast::JoinOperator::Left(ast::JoinConstraint::On(expression))
    | ast::JoinOperator::LeftOuter(ast::JoinConstraint::On(expression))) = operator
    else {
        return None;
    };
    let preserve_left = matches!(
        operator,
        ast::JoinOperator::Left(_) | ast::JoinOperator::LeftOuter(_)
    );
    let ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::Eq,
        right,
    } = expression
    else {
        return None;
    };
    let (left_slot, left_type) = resolve_hash_expression_slot(left, scope)?;
    let (right_slot, right_type) = resolve_hash_expression_slot(right, scope)?;
    if left_type.base != right_type.base
        || !matches!(
            left_type.base,
            BaseType::Bool
                | BaseType::Int2
                | BaseType::Int4
                | BaseType::Int8
                | BaseType::Text
                | BaseType::Varchar
                | BaseType::Bpchar
                | BaseType::Bytea
                | BaseType::Uuid
        )
    {
        return None;
    }
    if (left_start..right_start).contains(&right_slot)
        && (right_start..scope.columns.len()).contains(&left_slot)
    {
        return Some((right_slot, left_slot, preserve_left));
    }
    ((left_start..right_start).contains(&left_slot)
        && (right_start..scope.columns.len()).contains(&right_slot))
    .then_some((left_slot, right_slot, preserve_left))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_hash_expression_slot(
    expression: &ast::Expr,
    scope: &BoundScope,
) -> Option<(usize, PgType)> {
    match expression {
        ast::Expr::Identifier(identifier) => {
            scope.resolve_column(std::slice::from_ref(identifier)).ok()
        }
        ast::Expr::CompoundIdentifier(identifiers) => scope.resolve_column(identifiers).ok(),
        _ => None,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn visit_hash_join_chain_rows(
    state: &DatabaseState,
    table: &ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    starts: &[usize],
    hash_slots: &[(usize, usize, bool)],
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let mut rows = Vec::new();
    let first_end = starts.get(1).copied().unwrap_or(scope.columns.len());
    visit_table_factor_rows(
        state,
        &table.relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        starts[0],
        &mut |row| {
            rows.push(row[..first_end].to_vec());
            Ok(())
        },
    )?;
    for (index, (left_slot, right_slot, preserve_left)) in hash_slots.iter().copied().enumerate() {
        let right_start = starts[index + 1];
        let right_end = starts
            .get(index + 2)
            .copied()
            .unwrap_or(scope.columns.len());
        let mut right_rows = std::collections::HashMap::<JoinKey, Vec<Vec<Value>>>::new();
        visit_table_factor_rows(
            state,
            &table.joins[index].relation,
            scope,
            xid,
            snapshot,
            context,
            selection,
            right_start,
            &mut |row| {
                if let Some(key) = create_hash_join_key(&row[right_slot]) {
                    right_rows
                        .entry(key)
                        .or_default()
                        .push(row[right_start..right_end].to_vec());
                }
                Ok(())
            },
        )?;
        let mut joined = Vec::new();
        for left in rows {
            let matches =
                create_hash_join_key(&left[left_slot]).and_then(|key| right_rows.get(&key));
            if let Some(matches) = matches {
                joined.extend(matches.iter().map(|right| {
                    let mut row = left.clone();
                    row.extend_from_slice(right);
                    row
                }));
            } else if preserve_left {
                let mut row = left;
                row.resize(right_end, Value::Null);
                joined.push(row);
            }
        }
        rows = joined;
    }
    for row in rows {
        visit(&row)?;
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_hash_join_key(value: &Value) -> Option<JoinKey> {
    match value {
        Value::Null => None,
        Value::Bool(value) => Some(JoinKey::Bool(*value)),
        Value::Int2(value) => Some(JoinKey::Int2(*value)),
        Value::Int4(value) => Some(JoinKey::Int4(*value)),
        Value::Int8(value) => Some(JoinKey::Int8(*value)),
        Value::Text(value) => Some(JoinKey::Text(value.clone())),
        Value::Bytea(value) => Some(JoinKey::Bytea(value.clone())),
        Value::Uuid(value) => Some(JoinKey::Uuid(*value)),
        _ => None,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn visit_nested_loop_join_rows(
    state: &DatabaseState,
    table: &ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    starts: &[usize],
    right_sources: &[Vec<Vec<Value>>],
    index: usize,
    left: &[Value],
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let Some(join) = table.joins.get(index) else {
        return visit(left);
    };
    for right in &right_sources[index] {
        let row = left
            .iter()
            .zip(right)
            .map(|(left, right)| {
                if left.is_null() {
                    right.clone()
                } else {
                    left.clone()
                }
            })
            .collect::<Vec<_>>();
        if evaluate_join_condition(
            state,
            &join.join_operator,
            &row,
            scope,
            starts[0],
            starts[index + 1],
            xid,
            snapshot,
            context,
        )? {
            visit_nested_loop_join_rows(
                state,
                table,
                scope,
                xid,
                snapshot,
                context,
                starts,
                right_sources,
                index + 1,
                &row,
                visit,
            )?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn visit_table_factor_rows(
    state: &DatabaseState,
    factor: &ast::TableFactor,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    start: usize,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let ast::TableFactor::Table {
        name: table_name,
        args,
        ..
    } = factor
    else {
        unreachable!("streamable source is a table");
    };
    if args.is_some() {
        return reject_unsupported("table functions are not implemented");
    }
    let schema = state
        .catalog
        .require_table(&normalize_unqualified_object_name(table_name)?)?;
    let mut filters = Vec::new();
    if let Some(selection) = selection {
        collect_pushdown_filters(
            selection,
            scope,
            start,
            start + schema.columns.len(),
            &mut filters,
        );
    }
    let mut row = vec![Value::Null; scope.columns.len()];
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    for filter in &filters {
        let ast::Expr::BinaryOp {
            left,
            op: ast::BinaryOperator::Eq,
            right,
        } = filter
        else {
            continue;
        };
        let (column, value) = match (left.as_ref(), right.as_ref()) {
            (ast::Expr::Identifier(column), value) if is_point_lookup_value(value) => {
                (std::slice::from_ref(column), value)
            }
            (value, ast::Expr::Identifier(column)) if is_point_lookup_value(value) => {
                (std::slice::from_ref(column), value)
            }
            (ast::Expr::CompoundIdentifier(column), value) if is_point_lookup_value(value) => {
                (column.as_slice(), value)
            }
            (value, ast::Expr::CompoundIdentifier(column)) if is_point_lookup_value(value) => {
                (column.as_slice(), value)
            }
            _ => continue,
        };
        let Ok((slot, _)) = scope.resolve_column(column) else {
            continue;
        };
        if !(start..start + schema.columns.len()).contains(&slot) {
            continue;
        }
        let column = slot - start;
        if !table.has_unique_index(&[column])
            || resolve_operator_type(left, right, RowScope::Bound(scope))?
                != schema.columns[column].data_type.base
        {
            continue;
        }
        let value = evaluate_and_coerce(
            value,
            schema.columns[column].data_type.base,
            CastContext::Implicit,
            RowScope::Bound(scope),
            &row,
            context,
        )?;
        let Some(indexed_row) =
            table.find_unique_visible_row(&[column], &[value], snapshot, xid, &state.transactions)
        else {
            return Ok(());
        };
        row[start..start + indexed_row.len()].clone_from_slice(indexed_row);
        let passes = filters.iter().try_fold(true, |passes, filter| {
            if !passes {
                return Ok(false);
            }
            Ok(matches!(
                evaluate(filter, RowScope::Bound(scope), &row, context)?,
                Value::Bool(true)
            ))
        })?;
        if passes {
            visit(&row)?;
        }
        return Ok(());
    }
    for (_, chain) in table.iterate_version_chains() {
        let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions) else {
            continue;
        };
        row[start..start + version.row.len()].clone_from_slice(&version.row);
        let passes = filters.iter().try_fold(true, |passes, filter| {
            if !passes {
                return Ok(false);
            }
            Ok(matches!(
                evaluate(filter, RowScope::Bound(scope), &row, context)?,
                Value::Bool(true)
            ))
        })?;
        if passes {
            visit(&row)?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materialize_table_with_joins_rows(
    state: &DatabaseState,
    table: &ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    next_slot: &mut usize,
) -> Result<Vec<Vec<Value>>> {
    let left_start = *next_slot;
    let mut rows = materialize_table_factor_rows(
        state,
        &table.relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        next_slot,
    )?;
    for join in &table.joins {
        let right_start = *next_slot;
        let right_rows = materialize_table_factor_rows(
            state,
            &join.relation,
            scope,
            xid,
            snapshot,
            context,
            selection,
            next_slot,
        )?;
        let mut joined = Vec::new();
        let mut matched_right = vec![false; right_rows.len()];
        for left in &rows {
            let mut matched_left = false;
            for (index, right) in right_rows.iter().enumerate() {
                let row = left
                    .iter()
                    .zip(right)
                    .map(|(left, right)| {
                        if left.is_null() {
                            right.clone()
                        } else {
                            left.clone()
                        }
                    })
                    .collect::<Vec<_>>();
                if evaluate_join_condition(
                    state,
                    &join.join_operator,
                    &row,
                    scope,
                    left_start,
                    right_start,
                    xid,
                    snapshot,
                    context,
                )? {
                    matched_left = true;
                    matched_right[index] = true;
                    joined.push(row);
                }
            }
            if !matched_left
                && matches!(
                    join.join_operator,
                    ast::JoinOperator::Left(_)
                        | ast::JoinOperator::LeftOuter(_)
                        | ast::JoinOperator::FullOuter(_)
                )
            {
                joined.push(left.clone());
            }
        }
        if matches!(
            join.join_operator,
            ast::JoinOperator::Right(_)
                | ast::JoinOperator::RightOuter(_)
                | ast::JoinOperator::FullOuter(_)
        ) {
            joined.extend(
                right_rows
                    .iter()
                    .zip(matched_right)
                    .filter_map(|(row, matched)| (!matched).then_some(row.clone())),
            );
        }
        rows = joined;
    }
    Ok(rows)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materialize_table_factor_rows(
    state: &DatabaseState,
    factor: &ast::TableFactor,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    next_slot: &mut usize,
) -> Result<Vec<Vec<Value>>> {
    if let ast::TableFactor::NestedJoin {
        table_with_joins, ..
    } = factor
    {
        return materialize_table_with_joins_rows(
            state,
            table_with_joins,
            scope,
            xid,
            snapshot,
            context,
            selection,
            next_slot,
        );
    }
    if let ast::TableFactor::Derived {
        lateral,
        subquery,
        alias: Some(_),
        ..
    } = factor
    {
        if *lateral {
            return reject_unsupported("LATERAL derived tables are not implemented");
        }
        let StatementResult::Query(result) =
            execute_query(state, subquery, xid, snapshot, context)?
        else {
            unreachable!("derived query execution returns query rows");
        };
        let start = *next_slot;
        *next_slot += result.columns.len();
        return Ok(result
            .rows
            .into_iter()
            .map(|values| {
                let mut row = vec![Value::Null; scope.columns.len()];
                row[start..start + values.len()].clone_from_slice(&values);
                row
            })
            .collect());
    }
    let ast::TableFactor::Table {
        name: table_name,
        args,
        ..
    } = factor
    else {
        return reject_unsupported("FROM source is not implemented");
    };
    if args.is_some() {
        return reject_unsupported("table functions are not implemented");
    }
    let schema = state
        .catalog
        .require_table(&normalize_unqualified_object_name(table_name)?)?;
    let start = *next_slot;
    *next_slot += schema.columns.len();
    let mut filters = Vec::new();
    if let Some(selection) = selection {
        collect_pushdown_filters(
            selection,
            scope,
            start,
            start + schema.columns.len(),
            &mut filters,
        );
    }
    state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .iterate_version_chains()
        .filter_map(|(_, chain)| find_visible_version(chain, snapshot, xid, &state.transactions))
        .map(|version| {
            let mut row = vec![Value::Null; scope.columns.len()];
            row[start..start + version.row.len()].clone_from_slice(&version.row);
            let passes = filters.iter().try_fold(true, |passes, filter| {
                if !passes {
                    return Ok(false);
                }
                Ok(matches!(
                    evaluate(filter, RowScope::Bound(scope), &row, context)?,
                    Value::Bool(true)
                ))
            })?;
            Ok(passes.then_some(row))
        })
        .collect::<Result<Vec<_>>>()
        .map(|rows| rows.into_iter().flatten().collect())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn collect_pushdown_filters<'a>(
    expr: &'a ast::Expr,
    scope: &BoundScope,
    start: usize,
    end: usize,
    filters: &mut Vec<&'a ast::Expr>,
) {
    if let ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::And,
        right,
    } = expr
    {
        collect_pushdown_filters(left, scope, start, end, filters);
        collect_pushdown_filters(right, scope, start, end, filters);
        return;
    }
    if pushdown_filter_column(expr, scope).is_some_and(|slot| (start..end).contains(&slot)) {
        filters.push(expr);
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn pushdown_filter_column(expr: &ast::Expr, scope: &BoundScope) -> Option<usize> {
    let ast::Expr::BinaryOp { left, right, .. } = expr else {
        return None;
    };
    match (left.as_ref(), right.as_ref()) {
        (ast::Expr::Identifier(column), value) if is_point_lookup_value(value) => scope
            .resolve_column(std::slice::from_ref(column))
            .ok()
            .map(|(slot, _)| slot),
        (ast::Expr::CompoundIdentifier(columns), value) if is_point_lookup_value(value) => {
            scope.resolve_column(columns).ok().map(|(slot, _)| slot)
        }
        (value, ast::Expr::Identifier(column)) if is_point_lookup_value(value) => scope
            .resolve_column(std::slice::from_ref(column))
            .ok()
            .map(|(slot, _)| slot),
        (value, ast::Expr::CompoundIdentifier(columns)) if is_point_lookup_value(value) => {
            scope.resolve_column(columns).ok().map(|(slot, _)| slot)
        }
        _ => None,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn selection_is_fully_pushed(select: &ast::Select, scope: &BoundScope) -> bool {
    let [table] = select.from.as_slice() else {
        return false;
    };
    if !table.joins.is_empty() || !matches!(table.relation, ast::TableFactor::Table { .. }) {
        return false;
    }
    select
        .selection
        .as_ref()
        .is_some_and(|selection| filter_is_pushable(selection, scope, 0, scope.columns.len()))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn filter_is_pushable(expr: &ast::Expr, scope: &BoundScope, start: usize, end: usize) -> bool {
    if let ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::And,
        right,
    } = expr
    {
        return filter_is_pushable(left, scope, start, end)
            && filter_is_pushable(right, scope, start, end);
    }
    pushdown_filter_column(expr, scope).is_some_and(|slot| (start..end).contains(&slot))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_point_lookup_value(expr: &ast::Expr) -> bool {
    matches!(expr, ast::Expr::Value(_))
        || matches!(
            expr,
            ast::Expr::Cast {
                kind: ast::CastKind::Cast | ast::CastKind::DoubleColon,
                expr,
                format: None,
                ..
            } if matches!(expr.as_ref(), ast::Expr::Value(_))
        )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_join_condition(
    state: &DatabaseState,
    operator: &ast::JoinOperator,
    row: &[Value],
    scope: &BoundScope,
    left_start: usize,
    right_start: usize,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<bool> {
    let constraint = match operator {
        ast::JoinOperator::Join(constraint)
        | ast::JoinOperator::Inner(constraint)
        | ast::JoinOperator::CrossJoin(constraint)
        | ast::JoinOperator::Left(constraint)
        | ast::JoinOperator::LeftOuter(constraint)
        | ast::JoinOperator::Right(constraint)
        | ast::JoinOperator::RightOuter(constraint)
        | ast::JoinOperator::FullOuter(constraint) => constraint,
        _ => {
            return reject_unsupported("join type is not implemented");
        }
    };
    match constraint {
        ast::JoinConstraint::None => Ok(matches!(operator, ast::JoinOperator::CrossJoin(_))),
        ast::JoinConstraint::On(expression) => Ok(matches!(
            evaluate_query_expression(state, expression, scope, row, xid, snapshot, context,)?,
            Value::Bool(true)
        )),
        ast::JoinConstraint::Using(names) => evaluate_using_join_condition(
            names
                .iter()
                .map(normalize_unqualified_object_name)
                .collect::<Result<Vec<_>>>()?
                .as_slice(),
            row,
            scope,
            left_start,
            right_start,
        ),
        ast::JoinConstraint::Natural => {
            let names = scope.columns[left_start..right_start]
                .iter()
                .filter(|left| {
                    scope.columns[right_start..]
                        .iter()
                        .any(|right| right.name == left.name)
                })
                .map(|column| column.name.clone())
                .collect::<Vec<_>>();
            evaluate_using_join_condition(&names, row, scope, left_start, right_start)
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_using_join_condition(
    names: &[String],
    row: &[Value],
    scope: &BoundScope,
    left_start: usize,
    right_start: usize,
) -> Result<bool> {
    for name in names {
        let left = scope.columns[left_start..right_start]
            .iter()
            .find(|column| column.unqualified && column.name == *name)
            .expect("bound USING column must exist in left source");
        let right = scope.columns[right_start..]
            .iter()
            .find(|column| !column.unqualified && column.name == *name)
            .expect("bound USING column must exist in right source");
        let data_type = coercion::resolve_common_type(left.data_type.base, right.data_type.base)
            .expect("bound USING columns must have a common type");
        let left = coercion::coerce(
            row[left.slot].clone(),
            left.data_type.base,
            PgType::create(data_type),
            CastContext::Implicit,
        )?;
        let right = coercion::coerce(
            row[right.slot].clone(),
            right.data_type.base,
            PgType::create(data_type),
            CastContext::Implicit,
        )?;
        if left.is_null() || right.is_null() || left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_row_count(
    expr: &ast::Expr,
    clause: RowCountClause,
    context: &StatementExecutionContext,
) -> Result<Option<usize>> {
    if matches!(clause, RowCountClause::Limit)
        && matches!(expr, ast::Expr::Identifier(identifier) if identifier.quote_style.is_none() && identifier.value.eq_ignore_ascii_case("all"))
    {
        return Ok(None);
    }
    let schema = create_constant_expression_schema();
    let value = evaluate_and_coerce(
        expr,
        BaseType::Int8,
        CastContext::Implicit,
        RowScope::Table(&schema),
        &[],
        context,
    )
    .map_err(|error| {
        if error.sqlstate == SqlState::CannotCoerce {
            PgError::create(
                SqlState::DatatypeMismatch,
                match clause {
                    RowCountClause::Limit => "argument of LIMIT must be type bigint",
                    RowCountClause::Offset => "argument of OFFSET must be type bigint",
                },
            )
        } else {
            error
        }
    })?;
    match value {
        Value::Null => Ok(None),
        Value::Int8(value) if value >= 0 => Ok(Some(usize::try_from(value).unwrap_or(usize::MAX))),
        Value::Int8(_) => Err(PgError::create(
            match clause {
                RowCountClause::Limit => SqlState::InvalidRowCountInLimitClause,
                RowCountClause::Offset => SqlState::InvalidRowCountInResultOffsetClause,
            },
            match clause {
                RowCountClause::Limit => "LIMIT must not be negative",
                RowCountClause::Offset => "OFFSET must not be negative",
            },
        )),
        _ => unreachable!("row count was coerced to bigint"),
    }
}
