use sqlparser::ast::{self, VisitMut as _};
use std::collections::BTreeSet;

use crate::{
    ColumnMeta,
    catalog::ConstraintId,
    error::{PgError, Result, SqlState},
    executor::{
        DatabaseState,
        aggregates::{infer_aggregate_return_type, is_aggregate_function},
        expressions::{extract_number_literal, validate_equality_type},
        normalize_identifier,
        scope::{
            BoundScope, RowScope, bind_query_scope_with_outer, bind_select_scope,
            substitute_typed_subqueries,
        },
    },
    value::{PgType, Value},
};

use super::super::{
    distinct::{DistinctPlan, resolve_distinct_plan},
    expressions::infer_query_expression_type,
    ordering::{OrderKey, RowOrderSpec, resolve_order_specs},
    projection::{ProjectionSource, build_projection_plan, create_projection_expression},
};
use super::GroupingPlan;

struct QueryAggregateDetector {
    query_depth: usize,
    found: bool,
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
pub(in crate::executor) fn contains_query_aggregate(query: &ast::Query) -> bool {
    let mut query = query.clone();
    let mut detector = QueryAggregateDetector {
        query_depth: 0,
        found: false,
    };
    let _ = query.visit(&mut detector);
    detector.found
}

#[derive(Default)]
pub(in crate::executor::query) struct AggregateUsage {
    pub(in crate::executor::query) found: bool,
    outside_column: bool,
}

struct AggregateValidator<'a> {
    scope: &'a BoundScope,
    query_depth: usize,
    aggregate_depth: usize,
    usage: AggregateUsage,
    error: Option<PgError>,
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor::query) fn inspect_aggregate_usage(
    state: &DatabaseState,
    expression: &ast::Expr,
    scope: &BoundScope,
) -> Result<AggregateUsage> {
    let mut expression = substitute_typed_subqueries(&state.catalog, expression, scope)?;
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
                bind_query_scope_with_outer(self.catalog, select, outer)
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
            validate_equality_type(data_type.base)?;
            Ok((resolved, data_type))
        })
        .collect()
}

struct PrimaryKeyGrouping {
    constraint_id: ConstraintId,
    columns: Vec<(usize, PgType)>,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn collect_grouped_primary_keys(
    state: &DatabaseState,
    scope: &BoundScope,
    grouped_columns: &[(usize, PgType)],
) -> Vec<PrimaryKeyGrouping> {
    let mut grouped_primary_keys = Vec::new();
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
        let Some((constraint_id, primary_key)) =
            table
                .constraints
                .iter()
                .find_map(|constraint| match constraint {
                    crate::catalog::Constraint::PrimaryKey { id, columns, .. } => {
                        Some((*id, columns))
                    }
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
            grouped_primary_keys.push(PrimaryKeyGrouping {
                constraint_id,
                columns: relation_columns
                    .into_iter()
                    .filter(|column| !grouped_columns.iter().any(|(slot, _)| *slot == column.slot))
                    .map(|column| (column.slot, column.data_type))
                    .collect(),
            });
        }
    }
    grouped_primary_keys
}

fn validate_grouped_outputs(
    state: &DatabaseState,
    select: &ast::Select,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    scope: &BoundScope,
    expressions: &[(ast::Expr, PgType)],
    grouped_columns: &[(usize, PgType)],
) -> Result<()> {
    for projection in projections {
        validate_grouped_expression(
            state,
            &create_projection_expression(projection, scope),
            scope,
            expressions,
            grouped_columns,
        )?;
    }
    if let Some(having) = &select.having {
        validate_grouped_expression(state, having, scope, expressions, grouped_columns)?;
    }
    for order in order_specs {
        if let OrderKey::Input(_, expression) | OrderKey::Expression(expression) = order.key {
            validate_grouped_expression(state, expression, scope, expressions, grouped_columns)?;
        }
    }
    if let DistinctPlan::On {
        expressions: distinct_expressions,
        ..
    } = distinct
    {
        for expression in *distinct_expressions {
            validate_grouped_expression(state, expression, scope, expressions, grouped_columns)?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor::query) fn resolve_grouping_plan(
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
    let grouped_columns = expressions
        .iter()
        .filter_map(|(expression, _)| match expression {
            ast::Expr::Identifier(identifier) => {
                scope.resolve_column(std::slice::from_ref(identifier)).ok()
            }
            ast::Expr::CompoundIdentifier(identifiers) => scope.resolve_column(identifiers).ok(),
            _ => None,
        })
        .collect::<Vec<_>>();
    let grouped_primary_keys = collect_grouped_primary_keys(state, scope, &grouped_columns);
    let mut extended_columns = grouped_columns.clone();
    for primary_key in &grouped_primary_keys {
        extended_columns.extend(primary_key.columns.iter().copied());
    }

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
        validate_grouped_outputs(
            state,
            select,
            projections,
            order_specs,
            distinct,
            scope,
            &expressions,
            &extended_columns,
        )?;
    }
    let primary_key_dependencies = grouped_primary_keys
        .iter()
        .enumerate()
        .filter_map(|(excluded, primary_key)| {
            let mut columns = grouped_columns.clone();
            for (index, candidate) in grouped_primary_keys.iter().enumerate() {
                if index != excluded {
                    columns.extend(candidate.columns.iter().copied());
                }
            }
            validate_grouped_outputs(
                state,
                select,
                projections,
                order_specs,
                distinct,
                scope,
                &expressions,
                &columns,
            )
            .is_err()
            .then_some(primary_key.constraint_id)
        })
        .collect();
    Ok(GroupingPlan {
        expressions,
        enabled,
        primary_key_dependencies,
    })
}

struct PrimaryKeyDependencyCollector<'a> {
    state: &'a DatabaseState,
    dependencies: BTreeSet<ConstraintId>,
}

impl PrimaryKeyDependencyCollector<'_> {
    fn collect(&mut self, select: &ast::Select, query: Option<&ast::Query>) {
        let ast::GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
            return;
        };
        if group_by.is_empty() || !modifiers.is_empty() {
            return;
        }
        let Ok(scope) = bind_select_scope(self.state, select) else {
            return;
        };
        let Ok((projections, columns)) =
            build_projection_plan(self.state, &select.projection, &scope)
        else {
            return;
        };
        let order_specs = match query {
            Some(query) => {
                let Ok(order_specs) =
                    resolve_order_specs(self.state, query, &projections, &columns, &scope)
                else {
                    return;
                };
                order_specs
            }
            None => Vec::new(),
        };
        let Ok(distinct) = resolve_distinct_plan(
            self.state,
            select,
            &projections,
            &columns,
            &order_specs,
            &scope,
        ) else {
            return;
        };
        if let Ok(plan) = resolve_grouping_plan(
            self.state,
            select,
            group_by,
            &projections,
            &columns,
            &order_specs,
            &distinct,
            &scope,
        ) {
            self.dependencies.extend(plan.primary_key_dependencies);
        }
    }
}

impl ast::VisitorMut for PrimaryKeyDependencyCollector<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        if let ast::SetExpr::Select(select) = query.body.as_ref() {
            self.collect(select, Some(query));
        }
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &mut ast::Select) -> std::ops::ControlFlow<Self::Break> {
        self.collect(select, None);
        std::ops::ControlFlow::Continue(())
    }
}

pub(crate) fn collect_query_primary_key_dependencies(
    state: &DatabaseState,
    query: &ast::Query,
) -> BTreeSet<ConstraintId> {
    let mut query = query.clone();
    let mut collector = PrimaryKeyDependencyCollector {
        state,
        dependencies: BTreeSet::new(),
    };
    let _ = query.visit(&mut collector);
    collector.dependencies
}
