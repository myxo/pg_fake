use sqlparser::ast::{self, Spanned as _, VisitMut as _};
use std::{cell::Cell, collections::BTreeSet};

use crate::{
    catalog::ConstraintId,
    error::{PgError, Result, SqlState},
    executor::{
        DatabaseState, StatementContext,
        aggregates::{
            AggregateCall, AggregateInput, AggregateState, is_aggregate_function,
            parse_aggregate_call, prepare_aggregate_function_input,
        },
        equality::are_rows_not_distinct,
        expressions::{EvaluationCursor, resume_evaluation},
        from::visit_query_source_rows,
        scope::{BoundScope, RowScope, substitute_typed_subqueries},
        subqueries::evaluate_query_expression,
    },
    txn::{Snapshot, Xid},
    value::{BaseType, PgType, Value},
};

use super::{
    SelectRow,
    distinct::{DistinctKey, DistinctPlan},
    expressions::evaluate_select_expression,
    expressions::{contains_volatile_expression, prune_constant_cases},
    ordering::{OrderKey, RowOrderSpec},
    projection::ProjectionSource,
    select::evaluate_where_clause,
    windows::{WindowFunction, calculate_window_values, materialize_window_expression},
};

mod validation;
mod visitation;

pub(crate) use validation::collect_query_primary_key_dependencies;
pub(in crate::executor) use validation::contains_query_aggregate;
pub(super) use validation::{inspect_aggregate_usage, resolve_grouping_plan};
use visitation::sort_groups_by_postgres_visitation;

pub(super) struct GroupingPlan {
    pub(super) expressions: Vec<(ast::Expr, PgType)>,
    pub(super) enabled: bool,
    primary_key_dependencies: BTreeSet<ConstraintId>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum AggregateOwner {
    Projection(usize),
    Having,
    Order(usize),
    Distinct(usize),
}

pub(super) struct CollectedAggregateFunction {
    function: ast::Function,
    owner: AggregateOwner,
    volatile: bool,
}

#[derive(Clone)]
pub(in crate::executor) struct GroupedAggregateValue {
    function: ast::Function,
    owner: AggregateOwner,
    volatile: bool,
    value: Value,
    data_type: BaseType,
    used: Cell<bool>,
}

pub(in crate::executor) type GroupedAggregateValues = Vec<GroupedAggregateValue>;

#[derive(Clone)]
struct CollectedGroup {
    key: Vec<Value>,
    source: Option<Vec<Value>>,
    aggregate_states: Vec<Option<AggregateState>>,
}

#[derive(Clone)]
pub(crate) struct PreparedGrouping {
    occurrence: sqlparser::tokenizer::Span,
    sql: String,
    groups: Vec<CollectedGroup>,
    next: usize,
    cursor: EvaluationCursor,
    complete: bool,
}

#[derive(Clone)]
pub(crate) struct PreparedGroupOutput {
    occurrence: sqlparser::tokenizer::Span,
    sql: String,
    rows: Vec<SelectRow>,
    next: usize,
    cursor: EvaluationCursor,
}

struct AggregateMaterializer<'a> {
    values: &'a GroupedAggregateValues,
    owner: AggregateOwner,
    query_depth: usize,
    error: Option<PgError>,
}

struct AggregateCollector {
    functions: Vec<CollectedAggregateFunction>,
    owner: AggregateOwner,
    query_depth: usize,
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
        let prepared = self.values.iter().find(|prepared| {
            prepared.function == *function
                && if prepared.volatile {
                    prepared.owner == self.owner && !prepared.used.get()
                } else {
                    true
                }
        });
        let Some(prepared) = prepared else {
            self.error = Some(PgError::create(
                SqlState::InternalError,
                "aggregate value was not prepared",
            ));
            return std::ops::ControlFlow::Break(());
        };
        prepared.used.set(true);
        *expression = crate::analyzer::create_typed_literal(
            prepared.value.clone(),
            PgType::create(prepared.data_type),
        );
        std::ops::ControlFlow::Continue(())
    }
}

impl ast::VisitorMut for AggregateCollector {
    type Break = ();

    fn pre_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.query_depth -= 1;
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if self.query_depth != 0 {
            return std::ops::ControlFlow::Continue(());
        }
        let ast::Expr::Function(function) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        if is_aggregate_function(function) {
            let volatile = contains_volatile_expression(&ast::Expr::Function(function.clone()));
            if volatile
                || !self
                    .functions
                    .iter()
                    .any(|collected| collected.function == *function)
            {
                self.functions.push(CollectedAggregateFunction {
                    function: function.clone(),
                    owner: self.owner,
                    volatile,
                });
            }
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn materialize_aggregate_expression(
    state: &DatabaseState,
    expression: &ast::Expr,
    scope: &BoundScope,
    values: &GroupedAggregateValues,
    owner: AggregateOwner,
) -> Result<ast::Expr> {
    let mut expression = expression.clone();
    prune_constant_cases(&mut expression, Some((state, scope)))?;
    let mut materializer = AggregateMaterializer {
        values,
        owner,
        query_depth: 0,
        error: None,
    };
    let _ = expression.visit(&mut materializer);
    materializer.error.map_or(Ok(expression), Err)
}

pub(super) fn collect_group_aggregate_functions(
    state: &DatabaseState,
    select: &ast::Select,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    scope: &BoundScope,
) -> Result<Vec<CollectedAggregateFunction>> {
    let mut collector = AggregateCollector {
        functions: Vec::new(),
        owner: AggregateOwner::Having,
        query_depth: 0,
    };
    let mut visit = |owner, expression: &ast::Expr| {
        collector.owner = owner;
        let mut expression = expression.clone();
        prune_constant_cases(&mut expression, Some((state, scope)))?;
        let _ = expression.visit(&mut collector);
        Ok(())
    };
    for (index, projection) in projections.iter().enumerate() {
        if let ProjectionSource::Expression(expression) = projection {
            visit(AggregateOwner::Projection(index), expression)?;
        }
    }
    if let Some(having) = &select.having {
        visit(AggregateOwner::Having, having)?;
    }
    for (index, order) in order_specs.iter().enumerate() {
        if let OrderKey::Expression(expression) = order.key {
            visit(AggregateOwner::Order(index), expression)?;
        }
    }
    if let DistinctPlan::On { keys, .. } = distinct {
        for (index, key) in keys.iter().enumerate() {
            if let DistinctKey::Expression(expression) = key {
                visit(AggregateOwner::Distinct(index), expression)?;
            }
        }
    }
    Ok(collector.functions)
}

fn extract_aggregate_expressions(function: &ast::Function) -> Vec<&ast::Expr> {
    match &function.args {
        ast::FunctionArguments::List(arguments) => arguments
            .args
            .iter()
            .filter_map(|argument| match argument {
                ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expression)) => {
                    Some(expression)
                }
                _ => None,
            })
            .chain(arguments.clauses.iter().flat_map(|clause| {
                match clause {
                    ast::FunctionArgumentClause::OrderBy(expressions) => expressions
                        .iter()
                        .map(|expression| &expression.expr)
                        .collect(),
                    _ => Vec::new(),
                }
            }))
            .collect(),
        _ => Vec::new(),
    }
}

fn prepare_group_aggregate_input(
    state: &DatabaseState,
    original: &ast::Function,
    typed: &ast::Function,
    call: &AggregateCall<'_>,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<AggregateInput> {
    let original_arguments = extract_aggregate_expressions(original);
    let typed_arguments = extract_aggregate_expressions(typed);
    assert_eq!(original_arguments.len(), typed_arguments.len());
    let original_filter = original.filter.as_deref();
    prepare_aggregate_function_input(call, |typed_expression| {
        let expression = if typed
            .filter
            .as_deref()
            .is_some_and(|filter| std::ptr::eq(filter, typed_expression))
        {
            original_filter.expect("aggregate FILTER expression was validated")
        } else {
            let index = typed_arguments
                .iter()
                .position(|expression| std::ptr::eq(*expression, typed_expression))
                .expect("aggregate expression argument was validated");
            original_arguments[index]
        };
        evaluate_query_expression(state, expression, scope, row, xid, snapshot, context)
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn collect_grouped_select_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    grouped_expressions: &[(ast::Expr, PgType)],
    aggregate_functions: &[CollectedAggregateFunction],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Vec<(Vec<Value>, GroupedAggregateValues)>> {
    let typed_aggregate_functions = aggregate_functions
        .iter()
        .map(|collected| {
            let expression = substitute_typed_subqueries(
                &state.catalog,
                &ast::Expr::Function(collected.function.clone()),
                scope,
            )?;
            let ast::Expr::Function(function) = expression else {
                unreachable!("typed aggregate expression remains a function")
            };
            Ok(function)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut aggregate_calls: Vec<Option<AggregateCall<'_>>> =
        (0..aggregate_functions.len()).map(|_| None).collect();
    let groups = if grouped_expressions.is_empty() {
        vec![CollectedGroup {
            key: Vec::new(),
            source: None,
            aggregate_states: (0..aggregate_functions.len()).map(|_| None).collect(),
        }]
    } else {
        Vec::new()
    };
    let sql = format!("{:?} {select}", context.query_invocation);
    let mut cached = context
        .prepared_groups
        .lock()
        .expect("prepared groups mutex is poisoned");
    let mut prepared = cached
        .iter()
        .position(|entry| entry.occurrence == select.span() && entry.sql == sql)
        .map(|index| cached.remove(index))
        .unwrap_or_else(|| PreparedGrouping {
            occurrence: select.span(),
            sql,
            groups,
            next: 0,
            cursor: Default::default(),
            complete: false,
        });
    drop(cached);
    let mut source_index = 0;
    let evaluated = if prepared.complete {
        Ok(())
    } else {
        visit_query_source_rows(
            state,
            select,
            scope,
            xid,
            snapshot,
            context,
            select.selection.as_ref(),
            &mut |row, _origins| {
                let index = source_index;
                source_index += 1;
                if index < prepared.next {
                    return Ok(());
                }
                let evaluated = resume_evaluation(&mut prepared.cursor, context, |context| {
                    if !evaluate_where_clause(
                        state,
                        select.selection.as_ref(),
                        scope,
                        row,
                        xid,
                        snapshot,
                        context,
                    )? {
                        return Ok(None);
                    }
                    let key = grouped_expressions
                        .iter()
                        .map(|(expression, _)| {
                            evaluate_query_expression(
                                state, expression, scope, row, xid, snapshot, context,
                            )
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let inputs = aggregate_functions
                        .iter()
                        .zip(&typed_aggregate_functions)
                        .zip(&mut aggregate_calls)
                        .map(|((collected, typed), call)| {
                            if call.is_none() {
                                *call = Some(parse_aggregate_call(typed, RowScope::Bound(scope))?);
                            }
                            prepare_group_aggregate_input(
                                state,
                                &collected.function,
                                typed,
                                call.as_ref().unwrap(),
                                scope,
                                row,
                                xid,
                                snapshot,
                                context,
                            )
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Ok(Some((key, inputs)))
                })?;
                prepared.next += 1;
                let Some((key, inputs)) = evaluated else {
                    return Ok(());
                };
                let groups = &mut prepared.groups;
                let mut matching = None;
                for (index, group) in groups.iter().enumerate() {
                    if are_rows_not_distinct(&group.key, &key)? {
                        matching = Some(index);
                        break;
                    }
                }
                let index = match matching {
                    Some(index) => index,
                    None => {
                        groups.push(CollectedGroup {
                            key,
                            source: None,
                            aggregate_states: (0..aggregate_functions.len())
                                .map(|_| None)
                                .collect(),
                        });
                        groups.len() - 1
                    }
                };
                let group = &mut groups[index];
                for ((input, call), aggregate) in inputs
                    .into_iter()
                    .zip(&aggregate_calls)
                    .zip(&mut group.aggregate_states)
                {
                    let call = call.as_ref().expect("aggregate call was initialized");
                    aggregate
                        .get_or_insert_with(|| AggregateState::create(&call.descriptor))
                        .add_input(&call.descriptor, input);
                }
                if group.source.is_none() {
                    group.source = Some(row.to_vec());
                }
                Ok(())
            },
        )
    };
    prepared.complete = evaluated.is_ok();
    let groups = prepared.groups.clone();
    if context.capture_lock_queries {
        context
            .prepared_groups
            .lock()
            .expect("prepared groups mutex is poisoned")
            .push(prepared);
    }
    evaluated?;
    let groups = sort_groups_by_postgres_visitation(groups);
    groups
        .into_iter()
        .map(|group| {
            let source = group
                .source
                .unwrap_or_else(|| vec![Value::Null; scope.columns.len()]);
            let aggregate_values = aggregate_functions
                .iter()
                .zip(&typed_aggregate_functions)
                .zip(group.aggregate_states)
                .zip(&mut aggregate_calls)
                .map(|(((collected, typed), aggregate), call)| {
                    if call.is_none() {
                        *call = Some(parse_aggregate_call(typed, RowScope::Bound(scope))?);
                    }
                    let call = call.as_ref().expect("aggregate call was initialized");
                    let (value, data_type) = aggregate
                        .unwrap_or_else(|| AggregateState::create(&call.descriptor))
                        .finish(&call.descriptor)?;
                    Ok(GroupedAggregateValue {
                        function: collected.function.clone(),
                        owner: collected.owner,
                        volatile: collected.volatile,
                        value,
                        data_type,
                        used: Cell::new(false),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((source, aggregate_values))
        })
        .collect::<Result<Vec<_>>>()
}

pub(super) fn evaluate_group_having(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    source: &[Value],
    aggregate_values: &GroupedAggregateValues,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<bool> {
    let Some(having) = &select.having else {
        return Ok(true);
    };
    let expression = materialize_aggregate_expression(
        state,
        having,
        scope,
        aggregate_values,
        AggregateOwner::Having,
    )?;
    match evaluate_query_expression(state, &expression, scope, source, xid, snapshot, context)? {
        Value::Bool(value) => Ok(value),
        Value::Null => Ok(false),
        _ => unreachable!("HAVING expression was type-checked"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_grouped_select_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    projections: &[ProjectionSource<'_>],
    order_specs: &[RowOrderSpec<'_>],
    distinct: &DistinctPlan<'_>,
    grouped_expressions: &[(ast::Expr, PgType)],
    window_functions: &[WindowFunction],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Vec<SelectRow>> {
    let aggregate_functions = collect_group_aggregate_functions(
        state,
        select,
        projections,
        order_specs,
        distinct,
        scope,
    )?;
    let groups = collect_grouped_select_rows(
        state,
        select,
        scope,
        grouped_expressions,
        &aggregate_functions,
        xid,
        snapshot,
        context,
    )?;
    let sql = format!("{:?} {select}", context.query_invocation);
    let mut cached = context
        .prepared_group_outputs
        .lock()
        .expect("prepared group output mutex is poisoned");
    let mut prepared = cached
        .iter()
        .position(|entry| entry.occurrence == select.span() && entry.sql == sql)
        .map(|index| cached.remove(index))
        .unwrap_or_else(|| PreparedGroupOutput {
            occurrence: select.span(),
            sql,
            rows: Vec::new(),
            next: 0,
            cursor: Default::default(),
        });
    drop(cached);
    let groups = groups
        .into_iter()
        .map(|(row, aggregate_values)| {
            evaluate_group_having(
                state,
                select,
                scope,
                &row,
                &aggregate_values,
                xid,
                snapshot,
                context,
            )
            .map(|keep| keep.then_some((row, aggregate_values)))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let window_aggregate_values = groups
        .iter()
        .map(|(_, values)| values.clone())
        .collect::<Vec<_>>();
    let window_values = calculate_window_values(
        state,
        window_functions,
        scope,
        &groups
            .iter()
            .map(|(row, _)| row.clone())
            .collect::<Vec<_>>(),
        Some(&window_aggregate_values),
        xid,
        snapshot,
        context,
    )?;
    let evaluated = (|| {
        for ((row, aggregate_values), window_values) in
            groups.into_iter().zip(window_values).skip(prepared.next)
        {
            let output = resume_evaluation(&mut prepared.cursor, context, |context| {
                let values = projections
                    .iter()
                    .enumerate()
                    .map(|(index, projection)| match projection {
                        ProjectionSource::Expression(expression) => {
                            let expression = materialize_window_expression(
                                expression,
                                window_functions,
                                &window_values,
                                AggregateOwner::Projection(index),
                            );
                            evaluate_select_expression(
                                state,
                                &expression,
                                scope,
                                &row,
                                Some((&aggregate_values, AggregateOwner::Projection(index))),
                                xid,
                                snapshot,
                                context,
                            )
                        }
                        _ => super::projection::evaluate_projection_value(
                            state,
                            projection,
                            scope,
                            &row,
                            Some((&aggregate_values, AggregateOwner::Projection(index))),
                            xid,
                            snapshot,
                            context,
                        ),
                    })
                    .collect::<Result<Vec<_>>>()?;
                let keys = order_specs
                    .iter()
                    .enumerate()
                    .map(|(index, order)| match order.key {
                        OrderKey::Output(index) => Ok(values[index].clone()),
                        OrderKey::Input(slot, _) => Ok(row[slot].clone()),
                        OrderKey::Expression(expression) => {
                            let expression = materialize_window_expression(
                                expression,
                                window_functions,
                                &window_values,
                                AggregateOwner::Order(index),
                            );
                            evaluate_select_expression(
                                state,
                                &expression,
                                scope,
                                &row,
                                Some((&aggregate_values, AggregateOwner::Order(index))),
                                xid,
                                snapshot,
                                context,
                            )
                        }
                    })
                    .collect::<Result<Vec<_>>>()?;
                let distinct_keys = match distinct {
                    DistinctPlan::On { keys: distinct, .. } => distinct
                        .iter()
                        .enumerate()
                        .map(|(index, key)| match key {
                            DistinctKey::Output(index) => Ok(values[*index].clone()),
                            DistinctKey::Order(index) => Ok(keys[*index].clone()),
                            DistinctKey::Expression(expression) => {
                                let expression = materialize_window_expression(
                                    expression,
                                    window_functions,
                                    &window_values,
                                    AggregateOwner::Distinct(index),
                                );
                                evaluate_select_expression(
                                    state,
                                    &expression,
                                    scope,
                                    &row,
                                    Some((&aggregate_values, AggregateOwner::Distinct(index))),
                                    xid,
                                    snapshot,
                                    context,
                                )
                            }
                        })
                        .collect::<Result<Vec<_>>>()?,
                    DistinctPlan::None | DistinctPlan::Rows => Vec::new(),
                };
                Ok(Some(SelectRow {
                    origins: Vec::new(),
                    values,
                    keys,
                    distinct_keys,
                    deferred_source: None,
                    evaluated_projections: None,
                }))
            })?;
            prepared.next += 1;
            if let Some(row) = output {
                prepared.rows.push(row);
            }
        }
        Ok(())
    })();
    let rows = prepared.rows.clone();
    if context.capture_lock_queries {
        context
            .prepared_group_outputs
            .lock()
            .expect("prepared group output mutex is poisoned")
            .push(prepared);
    }
    evaluated?;
    Ok(rows)
}
