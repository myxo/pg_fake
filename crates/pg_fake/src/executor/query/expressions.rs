use super::grouping::{AggregateOwner, GroupedAggregateValues, materialize_aggregate_expression};
use crate::{
    error::{PgError, Result},
    executor::{
        DatabaseState, StatementContext, normalize_function_name,
        scope::{BoundScope, infer_expression_data_type},
        subqueries::evaluate_query_expression,
    },
    txn::{Snapshot, Xid},
    value::{PgType, Value},
};
use sqlparser::ast::{self, VisitMut as _};

struct ConstantCasePruner<'a> {
    type_context: Option<(&'a DatabaseState, &'a BoundScope)>,
    error: Option<PgError>,
}

struct BoundExpressionNormalizer<'a> {
    scope: &'a BoundScope,
    query_depth: usize,
    error: Option<PgError>,
}

impl ast::VisitorMut for ConstantCasePruner<'_> {
    type Break = ();

    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        loop {
            if !matches!(expression, ast::Expr::Case { operand: None, .. }) {
                break;
            }
            let data_type = if let Some((state, scope)) = self.type_context {
                match infer_query_expression_type(state, expression, scope) {
                    Ok(data_type) => Some(data_type),
                    Err(error) => {
                        self.error = Some(error);
                        return std::ops::ControlFlow::Break(());
                    }
                }
            } else {
                None
            };
            let ast::Expr::Case {
                operand: None,
                conditions,
                else_result,
                ..
            } = expression
            else {
                break;
            };
            if conditions.is_empty() {
                break;
            }
            let mut retained = Vec::new();
            let mut terminal = None;
            for condition in std::mem::take(conditions) {
                match &condition.condition {
                    ast::Expr::Value(value)
                        if matches!(value.value, ast::Value::Boolean(false) | ast::Value::Null) => {
                    }
                    ast::Expr::Value(value) if value.value == ast::Value::Boolean(true) => {
                        terminal = Some(condition.result);
                        break;
                    }
                    _ => retained.push(condition),
                }
            }
            if retained.is_empty() {
                let replacement = terminal
                    .or_else(|| else_result.take().map(|result| *result))
                    .unwrap_or_else(|| ast::Expr::Value(ast::Value::Null.into()));
                *expression = data_type.map_or(replacement.clone(), |data_type| {
                    crate::analyzer::create_typed_cast(replacement, data_type)
                });
                continue;
            }
            *conditions = retained;
            if let Some(terminal) = terminal {
                *else_result = Some(Box::new(terminal));
            }
            break;
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
pub(super) fn compare_bound_expressions(
    left: &ast::Expr,
    right: &ast::Expr,
    scope: &BoundScope,
) -> Result<bool> {
    Ok(normalize_bound_expression(left, scope)? == normalize_bound_expression(right, scope)?)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_select_expression(
    state: &DatabaseState,
    expression: &ast::Expr,
    scope: &BoundScope,
    row: &[Value],
    aggregate_values: Option<(&GroupedAggregateValues, AggregateOwner)>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Value> {
    let materialized;
    let expression = if let Some((values, owner)) = aggregate_values {
        materialized = materialize_aggregate_expression(state, expression, scope, values, owner)?;
        &materialized
    } else {
        expression
    };
    evaluate_query_expression(state, expression, scope, row, xid, snapshot, context)
}

pub(in crate::executor) fn contains_volatile_expression(expression: &ast::Expr) -> bool {
    let mut expression = expression.clone();
    prune_constant_cases(&mut expression, None).expect("untyped CASE pruning cannot fail");
    let mut found = crate::advisory::contains_advisory_function(&expression);
    let _ = ast::visit_expressions(&expression, |nested| {
        let ast::Expr::Function(function) = nested else {
            return std::ops::ControlFlow::Continue(());
        };
        if normalize_function_name(&function.name).is_ok_and(|name| {
            matches!(
                name.as_str(),
                "gen_random_uuid"
                    | "uuidv4"
                    | "uuidv7"
                    | "clock_timestamp"
                    | "nextval"
                    | "currval"
                    | "lastval"
                    | "setval"
            )
        }) {
            found = true;
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    });
    found
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn infer_query_expression_type(
    state: &DatabaseState,
    expr: &ast::Expr,
    scope: &BoundScope,
) -> Result<PgType> {
    infer_expression_data_type(&state.catalog, expr, scope)
}

pub(super) fn prune_constant_cases(
    expression: &mut ast::Expr,
    type_context: Option<(&DatabaseState, &BoundScope)>,
) -> Result<()> {
    let mut pruner = ConstantCasePruner {
        type_context,
        error: None,
    };
    let _ = expression.visit(&mut pruner);
    pruner.error.map_or(Ok(()), Err)
}
