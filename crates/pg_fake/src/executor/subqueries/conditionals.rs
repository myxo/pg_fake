use super::{
    BoundScope, DatabaseState, Result, Snapshot, StatementContext, Value, Xid,
    evaluate_query_expression,
};
use crate::executor::scope::infer_expression_data_type;
use sqlparser::ast;

pub(super) fn materialize_selected_branch(
    state: &DatabaseState,
    expression: &ast::Expr,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Option<ast::Expr>> {
    let scope = BoundScope {
        columns: Vec::new(),
    };
    let evaluate = |expr: &ast::Expr| {
        evaluate_query_expression(state, expr, &scope, &[], xid, snapshot, context)
    };
    let selected = match expression {
        ast::Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            let operand = operand
                .as_ref()
                .map(|operand| {
                    let data_type = infer_expression_data_type(&state.catalog, operand, &scope)?;
                    Ok(crate::analyzer::create_typed_literal(
                        evaluate(operand)?,
                        data_type,
                    ))
                })
                .transpose()?;
            let mut selected = else_result.as_deref().cloned();
            for condition in conditions {
                let comparison = if let Some(operand) = &operand {
                    ast::Expr::BinaryOp {
                        left: Box::new(operand.clone()),
                        op: ast::BinaryOperator::Eq,
                        right: Box::new(condition.condition.clone()),
                    }
                } else {
                    condition.condition.clone()
                };
                if matches!(evaluate(&comparison)?, Value::Bool(true)) {
                    selected = Some(condition.result.clone());
                    break;
                }
            }
            selected
        }
        ast::Expr::Function(function)
            if function.name.to_string().eq_ignore_ascii_case("coalesce") =>
        {
            let ast::FunctionArguments::List(arguments) = &function.args else {
                return Ok(None);
            };
            let mut selected = None;
            for argument in &arguments.args {
                let ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument)) = argument
                else {
                    return Ok(None);
                };
                let value = evaluate(argument)?;
                if !value.is_null() {
                    selected = Some(crate::analyzer::create_typed_literal(
                        value,
                        infer_expression_data_type(&state.catalog, argument, &scope)?,
                    ));
                    break;
                }
            }
            selected
        }
        ast::Expr::BinaryOp { left, op, right }
            if matches!(op, ast::BinaryOperator::And | ast::BinaryOperator::Or) =>
        {
            let left = evaluate(left)?;
            let value = match (op, &left) {
                (ast::BinaryOperator::And, Value::Bool(false))
                | (ast::BinaryOperator::Or, Value::Bool(true)) => left,
                _ => crate::executor::arithmetic::evaluate_boolean_operator(
                    op,
                    left,
                    evaluate(right)?,
                )?,
            };
            Some(crate::analyzer::create_typed_literal(
                value,
                crate::value::PgType::create(crate::value::BaseType::Bool),
            ))
        }
        _ => return Ok(None),
    };
    let data_type = infer_expression_data_type(&state.catalog, expression, &scope)?;
    Ok(Some(match selected {
        Some(selected) => crate::analyzer::create_typed_cast(selected, data_type),
        None => crate::analyzer::create_typed_literal(Value::Null, data_type),
    }))
}
