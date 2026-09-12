use super::{
    StatementContext,
    arithmetic::{
        evaluate_boolean_operator, evaluate_distinctness, evaluate_numeric_operator,
        evaluate_temporal_arithmetic, evaluate_unary_operator,
    },
    json,
    scope::RowScope,
};
use crate::{
    catalog::{TableId, TableSchema},
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, PgType, Value},
};
use sqlparser::ast;

mod comparisons;
mod functions;
mod literals;
mod types;

pub(super) use comparisons::{
    compare_values, evaluate_comparison, validate_equality_type, validate_ordering_type,
};
pub(super) use functions::{infer_window_return_type, validate_function_argument};
pub(super) use literals::{evaluate_literal, extract_number_literal};
pub(crate) use literals::{extract_unknown_string_literal, is_null_literal};
pub(super) use types::resolve_operator_type;
pub(crate) use types::{infer_expression_data_type, infer_expression_type};

use comparisons::{evaluate_membership, evaluate_quantified};
use functions::{evaluate_function, extract_datetime_field};
use literals::parse_integer_literal;
use types::resolve_expression_list_type;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_assignment_expression(
    expr: &ast::Expr,
    target: PgType,
    schema: &TableSchema,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    if let Some(text) = extract_unknown_string_literal(expr) {
        coercion::coerce_unknown(text, target, CastContext::Assignment)
    } else {
        coercion::coerce(
            evaluate(expr, RowScope::Table(schema), row, context)?,
            infer_expression_type(expr, RowScope::Table(schema))?,
            target,
            CastContext::Assignment,
        )
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn create_constant_expression_schema() -> TableSchema {
    TableSchema {
        id: TableId(0),
        schema_id: crate::catalog::SchemaId(0),
        name: String::new(),
        columns: Vec::new(),
        constraints: Vec::new(),
        indexes: Vec::new(),
        triggers: Vec::new(),
        persistence: crate::catalog::TablePersistence::Permanent,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate(
    expr: &ast::Expr,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    context.check_timeout()?;
    match expr {
        ast::Expr::Identifier(column) => {
            schema.resolve_column_value(std::slice::from_ref(column), row)
        }
        ast::Expr::CompoundIdentifier(columns) => schema.resolve_column_value(columns, row),
        ast::Expr::Value(_) | ast::Expr::TypedString(_) => evaluate_literal(expr),
        ast::Expr::Nested(expr) => evaluate(expr, schema, row, context),
        ast::Expr::UnaryOp { op, expr } => {
            if matches!(op, ast::UnaryOperator::Minus)
                && let Some(value) = extract_number_literal(expr)
                && !value.contains(['.', 'e', 'E'])
            {
                return parse_integer_literal(&format!("-{value}"));
            }
            evaluate_unary_operator(*op, evaluate(expr, schema, row, context)?)
        }
        ast::Expr::Array(array) => {
            let values = array
                .elem
                .iter()
                .map(|element| {
                    let value = evaluate_and_coerce(
                        element,
                        BaseType::Text,
                        CastContext::Implicit,
                        schema,
                        row,
                        context,
                    )?;
                    Ok(match value {
                        Value::Null => None,
                        Value::Text(value) => Some(value),
                        _ => unreachable!(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Value::TextArray(values))
        }
        ast::Expr::Interval(interval)
            if interval.leading_field.is_none()
                && interval.leading_precision.is_none()
                && interval.last_field.is_none()
                && interval.fractional_seconds_precision.is_none() =>
        {
            let text = extract_unknown_string_literal(&interval.value)
                .expect("interval expression type was checked");
            Value::parse(BaseType::Interval, text)
        }
        ast::Expr::BinaryOp { left, op, right } => {
            if let Some((l, r, result)) = json::infer_json_operator(op, left, right, schema)? {
                return json::evaluate_json_operator(
                    op,
                    evaluate_and_coerce(left, l, CastContext::Implicit, schema, row, context)?,
                    evaluate_and_coerce(right, r, CastContext::Implicit, schema, row, context)?,
                    result,
                );
            }
            let left_type = infer_expression_type(left, schema)?;
            let right_type = infer_expression_type(right, schema)?;
            if matches!(
                op,
                ast::BinaryOperator::Plus
                    | ast::BinaryOperator::Minus
                    | ast::BinaryOperator::Multiply
                    | ast::BinaryOperator::Divide
            ) && (left_type == BaseType::Interval || right_type == BaseType::Interval)
            {
                let left = evaluate(left, schema, row, context)?;
                let right = evaluate(right, schema, row, context)?;
                if left.is_null() || right.is_null() {
                    return Ok(Value::Null);
                }
                return evaluate_temporal_arithmetic(op, left, right);
            }
            let target = match op {
                ast::BinaryOperator::And | ast::BinaryOperator::Or => BaseType::Bool,
                ast::BinaryOperator::Plus
                | ast::BinaryOperator::Minus
                | ast::BinaryOperator::Multiply
                | ast::BinaryOperator::Divide
                | ast::BinaryOperator::Modulo => resolve_operator_type(left, right, schema)?,
                _ => resolve_operator_type(left, right, schema)?,
            };
            let left =
                evaluate_and_coerce(left, target, CastContext::Implicit, schema, row, context)?;
            if matches!(
                (op, &left),
                (ast::BinaryOperator::And, Value::Bool(false))
                    | (ast::BinaryOperator::Or, Value::Bool(true))
            ) {
                return Ok(left);
            }
            let right =
                evaluate_and_coerce(right, target, CastContext::Implicit, schema, row, context)?;
            match op {
                ast::BinaryOperator::Plus
                | ast::BinaryOperator::Minus
                | ast::BinaryOperator::Multiply
                | ast::BinaryOperator::Divide
                | ast::BinaryOperator::Modulo => {
                    if left.is_null() || right.is_null() {
                        Ok(Value::Null)
                    } else {
                        evaluate_numeric_operator(op, left, right)
                    }
                }
                ast::BinaryOperator::Eq
                | ast::BinaryOperator::NotEq
                | ast::BinaryOperator::Gt
                | ast::BinaryOperator::Lt
                | ast::BinaryOperator::GtEq
                | ast::BinaryOperator::LtEq => {
                    if left.is_null() || right.is_null() {
                        Ok(Value::Null)
                    } else {
                        evaluate_comparison(op, &left, &right)
                    }
                }
                ast::BinaryOperator::And | ast::BinaryOperator::Or => {
                    evaluate_boolean_operator(op, left, right)
                }
                ast::BinaryOperator::PGRegexMatch => {
                    let (Value::Text(value), Value::Text(pattern)) = (left, right) else {
                        return Ok(Value::Null);
                    };
                    validate_regex_repetition_bounds(&pattern)?;
                    let regex = regex::Regex::new(&pattern).map_err(|error| {
                        PgError::create(SqlState::InvalidRegularExpression, error.to_string())
                    })?;
                    Ok(Value::Bool(regex.is_match(&value)))
                }
                _ => reject_unsupported("operator is not implemented"),
            }
        }
        ast::Expr::IsNull(expr) => Ok(Value::Bool(evaluate(expr, schema, row, context)?.is_null())),
        ast::Expr::IsNotNull(expr) => Ok(Value::Bool(
            !evaluate(expr, schema, row, context)?.is_null(),
        )),
        ast::Expr::InList {
            expr,
            list,
            negated,
        } => evaluate_membership(expr, list, *negated, schema, row, context),
        ast::Expr::AnyOp {
            left,
            compare_op,
            right,
            ..
        } => evaluate_quantified(left, compare_op, right, false, schema, row, context),
        ast::Expr::AllOp {
            left,
            compare_op,
            right,
        } => evaluate_quantified(left, compare_op, right, true, schema, row, context),
        ast::Expr::IsTrue(expr) => Ok(Value::Bool(matches!(
            evaluate_and_coerce(
                expr,
                BaseType::Bool,
                CastContext::Implicit,
                schema,
                row,
                context
            )?,
            Value::Bool(true)
        ))),
        ast::Expr::IsFalse(expr) => Ok(Value::Bool(matches!(
            evaluate_and_coerce(
                expr,
                BaseType::Bool,
                CastContext::Implicit,
                schema,
                row,
                context
            )?,
            Value::Bool(false)
        ))),
        ast::Expr::IsUnknown(expr) => Ok(Value::Bool(
            evaluate_and_coerce(
                expr,
                BaseType::Bool,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?
            .is_null(),
        )),
        ast::Expr::IsDistinctFrom(left, right) | ast::Expr::IsNotDistinctFrom(left, right) => {
            let target = resolve_operator_type(left, right, schema)?;
            evaluate_distinctness(
                evaluate_and_coerce(left, target, CastContext::Implicit, schema, row, context)?,
                evaluate_and_coerce(right, target, CastContext::Implicit, schema, row, context)?,
                matches!(expr, ast::Expr::IsNotDistinctFrom(_, _)),
            )
        }
        ast::Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            let result_type = resolve_expression_list_type(
                conditions
                    .iter()
                    .map(|condition| &condition.result)
                    .chain(else_result.as_deref())
                    .collect::<Vec<_>>()
                    .as_slice(),
                schema,
            )?;
            let operand = operand.as_deref();
            for condition in conditions {
                let matches = if let Some(operand) = &operand {
                    let target = resolve_operator_type(operand, &condition.condition, schema)?;
                    let operand = evaluate_and_coerce(
                        operand,
                        target,
                        CastContext::Implicit,
                        schema,
                        row,
                        context,
                    )?;
                    let condition = evaluate_and_coerce(
                        &condition.condition,
                        target,
                        CastContext::Implicit,
                        schema,
                        row,
                        context,
                    )?;
                    if operand.is_null() || condition.is_null() {
                        false
                    } else {
                        matches!(
                            evaluate_comparison(&ast::BinaryOperator::Eq, &operand, &condition)?,
                            Value::Bool(true)
                        )
                    }
                } else {
                    matches!(
                        evaluate(&condition.condition, schema, row, context)?,
                        Value::Bool(true)
                    )
                };
                if matches {
                    return evaluate_and_coerce(
                        &condition.result,
                        result_type,
                        CastContext::Implicit,
                        schema,
                        row,
                        context,
                    );
                }
            }
            match else_result {
                Some(result) => evaluate_and_coerce(
                    result,
                    result_type,
                    CastContext::Implicit,
                    schema,
                    row,
                    context,
                ),
                None => Ok(Value::Null),
            }
        }
        ast::Expr::Function(function) => evaluate_function(function, schema, row, context),
        ast::Expr::Cast {
            kind,
            expr,
            data_type,
            format,
            ..
        } => {
            if !matches!(kind, ast::CastKind::Cast | ast::CastKind::DoubleColon) || format.is_some()
            {
                return reject_unsupported("cast variant is not implemented");
            }
            let target = coercion::convert_ast_data_type(data_type)?;
            if let Some(text) = extract_unknown_string_literal(expr) {
                coercion::coerce_unknown(text, target, CastContext::Explicit)
            } else {
                coercion::coerce(
                    evaluate(expr, schema, row, context)?,
                    infer_expression_type(expr, schema)?,
                    target,
                    CastContext::Explicit,
                )
            }
        }
        ast::Expr::Extract { field, expr, .. } => {
            extract_datetime_field(field.clone(), evaluate(expr, schema, row, context)?)
        }
        _ => reject_unsupported("expression is not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_and_coerce(
    expression: &ast::Expr,
    target: BaseType,
    context: CastContext,
    schema: RowScope<'_>,
    row: &[Value],
    execution: &StatementContext,
) -> Result<Value> {
    if let Some(text) = extract_unknown_string_literal(expression) {
        coercion::coerce_unknown(text, PgType::create(target), context)
    } else {
        let source = infer_expression_type(expression, schema)?;
        coercion::coerce(
            evaluate(expression, schema, row, execution)?,
            source,
            PgType::create(target),
            context,
        )
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validate_regex_repetition_bounds(pattern: &str) -> Result<()> {
    let bytes = pattern.as_bytes();
    let mut index = 0;
    let mut in_character_class = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'[' if !in_character_class => {
                in_character_class = true;
                index += 1;
            }
            b']' if in_character_class => {
                in_character_class = false;
                index += 1;
            }
            b'{' if !in_character_class => {
                index += 1;
                let mut bound = 0_u16;
                let mut has_digits = false;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    has_digits = true;
                    bound = bound
                        .saturating_mul(10)
                        .saturating_add(u16::from(bytes[index] - b'0'));
                    if bound > 255 {
                        return Err(PgError::create(
                            SqlState::InvalidRegularExpression,
                            "invalid repetition count",
                        ));
                    }
                    index += 1;
                }
                if has_digits && index < bytes.len() && bytes[index] == b',' {
                    index += 1;
                    bound = 0;
                    while index < bytes.len() && bytes[index].is_ascii_digit() {
                        bound = bound
                            .saturating_mul(10)
                            .saturating_add(u16::from(bytes[index] - b'0'));
                        if bound > 255 {
                            return Err(PgError::create(
                                SqlState::InvalidRegularExpression,
                                "invalid repetition count",
                            ));
                        }
                        index += 1;
                    }
                }
            }
            _ => index += 1,
        }
    }
    Ok(())
}
