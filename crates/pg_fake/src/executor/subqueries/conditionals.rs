use super::{
    BoundScope, DatabaseState, Result, Snapshot, StatementContext, Value, Xid,
    evaluate_query_expression,
};
use crate::executor::scope::infer_expression_data_type;
use sqlparser::ast;

pub(super) fn materialize_selected_branch(
    state: &DatabaseState,
    expression: &mut ast::Expr,
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
            let evaluated_operand = operand
                .as_ref()
                .map(|operand| {
                    let data_type = infer_expression_data_type(&state.catalog, operand, &scope)?;
                    Ok(crate::analyzer::create_typed_literal(
                        evaluate(operand)?,
                        data_type,
                    ))
                })
                .transpose()?;
            if let Some(evaluated_operand) = evaluated_operand {
                for condition in conditions.iter_mut() {
                    condition.condition = ast::Expr::BinaryOp {
                        left: Box::new(evaluated_operand.clone()),
                        op: ast::BinaryOperator::Eq,
                        right: Box::new(condition.condition.clone()),
                    };
                }
                *operand = None;
            }
            let mut selected = else_result.as_deref().cloned();
            for condition in conditions {
                let value = evaluate(&condition.condition)?;
                condition.condition = crate::analyzer::create_typed_literal(
                    value.clone(),
                    crate::value::PgType::create(crate::value::BaseType::Bool),
                );
                if matches!(value, Value::Bool(true)) {
                    selected = Some(condition.result.clone());
                    break;
                }
            }
            selected
        }
        ast::Expr::Function(function)
            if function.name.to_string().eq_ignore_ascii_case("coalesce") =>
        {
            let ast::FunctionArguments::List(arguments) = &mut function.args else {
                return Ok(None);
            };
            let mut selected = None;
            for argument in &mut arguments.args {
                let ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument)) = argument
                else {
                    return Ok(None);
                };
                let value = evaluate(argument)?;
                let data_type = infer_expression_data_type(&state.catalog, argument, &scope)?;
                *argument = crate::analyzer::create_typed_literal(value.clone(), data_type);
                if !value.is_null() {
                    selected = Some(argument.clone());
                    break;
                }
            }
            selected
        }
        ast::Expr::BinaryOp { left, op, right }
            if matches!(op, ast::BinaryOperator::And | ast::BinaryOperator::Or) =>
        {
            let left_value = evaluate(left)?;
            **left = crate::analyzer::create_typed_literal(
                left_value.clone(),
                crate::value::PgType::create(crate::value::BaseType::Bool),
            );
            let value = match (&*op, &left_value) {
                (ast::BinaryOperator::And, Value::Bool(false))
                | (ast::BinaryOperator::Or, Value::Bool(true)) => left_value,
                _ => crate::executor::arithmetic::evaluate_boolean_operator(
                    op,
                    left_value,
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
