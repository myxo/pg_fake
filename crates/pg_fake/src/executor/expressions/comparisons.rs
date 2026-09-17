use super::{
    evaluate_and_coerce,
    types::{infer_expression_type, resolve_operator_type},
};
use crate::executor::{
    StatementContext,
    arithmetic::{evaluate_boolean_operator, evaluate_unary_operator},
    scope::RowScope,
};
use crate::{
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState},
    value::{BaseType, DAYS_PER_MONTH, MICROSECONDS_PER_DAY, Value},
};
use sqlparser::ast;
use std::cmp::Ordering;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn evaluate_comparison(
    operator: &ast::BinaryOperator,
    left: &Value,
    right: &Value,
) -> Result<Value> {
    let ordering = compare_values(left, right)?;
    Ok(Value::Bool(match operator {
        ast::BinaryOperator::Eq => ordering == Ordering::Equal,
        ast::BinaryOperator::NotEq => ordering != Ordering::Equal,
        ast::BinaryOperator::Gt => ordering == Ordering::Greater,
        ast::BinaryOperator::Lt => ordering == Ordering::Less,
        ast::BinaryOperator::GtEq => ordering != Ordering::Less,
        ast::BinaryOperator::LtEq => ordering != Ordering::Greater,
        _ => unreachable!("evaluate_comparison operator was checked by caller"),
    }))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn compare_values(left: &Value, right: &Value) -> Result<Ordering> {
    Ok(match (left, right) {
        (Value::Bool(left), Value::Bool(right)) => left.cmp(right),
        (Value::Int2(left), Value::Int2(right)) => left.cmp(right),
        (Value::Int4(left), Value::Int4(right)) => left.cmp(right),
        (Value::Oid(left), Value::Oid(right)) => left.cmp(right),
        (Value::Int8(left), Value::Int8(right)) => left.cmp(right),
        (Value::Float4(left), Value::Float4(right)) => compare_float4(*left, *right),
        (Value::Float8(left), Value::Float8(right)) => compare_float8(*left, *right),
        (Value::Numeric(left), Value::Numeric(right)) => left.cmp(right),
        (Value::Text(left), Value::Text(right)) => left.cmp(right),
        (Value::Bytea(left), Value::Bytea(right)) => left.cmp(right),
        (Value::Uuid(left), Value::Uuid(right)) => left.cmp(right),
        (Value::Date(left), Value::Date(right)) => left.cmp(right),
        (Value::Time(left), Value::Time(right)) => left.cmp(right),
        (Value::Timestamp(left), Value::Timestamp(right)) => left.cmp(right),
        (Value::TimestampTz(left), Value::TimestampTz(right)) => left.cmp(right),
        (Value::Interval(left), Value::Interval(right)) => {
            let left = i128::from(left.months)
                * i128::from(DAYS_PER_MONTH)
                * i128::from(MICROSECONDS_PER_DAY)
                + i128::from(left.days) * i128::from(MICROSECONDS_PER_DAY)
                + i128::from(left.micros);
            let right = i128::from(right.months)
                * i128::from(DAYS_PER_MONTH)
                * i128::from(MICROSECONDS_PER_DAY)
                + i128::from(right.days) * i128::from(MICROSECONDS_PER_DAY)
                + i128::from(right.micros);
            left.cmp(&right)
        }
        (Value::Json(_), Value::Json(_)) => {
            return Err(create_missing_operator_error(BaseType::Json));
        }
        (Value::Jsonb(left), Value::Jsonb(right)) => left.compare(right),
        (Value::PgLsn(left), Value::PgLsn(right)) => left.cmp(right),
        (Value::Regclass(left), Value::Regclass(right)) => left.cmp(right),
        _ => {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "operator has incompatible types",
            ));
        }
    })
}

pub(in crate::executor) fn validate_equality_type(data_type: BaseType) -> Result<()> {
    if matches!(data_type, BaseType::Json | BaseType::Void) {
        Err(create_missing_operator_error(data_type))
    } else {
        Ok(())
    }
}

pub(in crate::executor) fn validate_ordering_type(data_type: BaseType) -> Result<()> {
    validate_equality_type(data_type)
}

pub(super) fn validate_comparison_type(
    operator: &ast::BinaryOperator,
    data_type: BaseType,
) -> Result<()> {
    if matches!(
        operator,
        ast::BinaryOperator::Eq | ast::BinaryOperator::NotEq
    ) {
        validate_equality_type(data_type)
    } else {
        validate_ordering_type(data_type)
    }
}

pub(super) fn create_missing_operator_error(data_type: BaseType) -> PgError {
    PgError::create(
        SqlState::UndefinedFunction,
        format!(
            "operator does not exist for type {}",
            data_type.get_postgres_name()
        ),
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn compare_float4(left: f32, right: f32) -> Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => left
            .partial_cmp(&right)
            .expect("finite floats are comparable"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn compare_float8(left: f64, right: f64) -> Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => left
            .partial_cmp(&right)
            .expect("finite floats are comparable"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn validate_membership_types(
    expr: &ast::Expr,
    list: &[ast::Expr],
    schema: RowScope<'_>,
) -> Result<()> {
    let left = extract_row_fields(expr);
    for field in left {
        validate_equality_type(infer_expression_type(field, schema)?)?;
    }
    for candidate in list {
        validate_row_comparison_types(expr, candidate, &ast::BinaryOperator::Eq, schema)?;
    }
    Ok(())
}

pub(super) fn validate_row_comparison_types(
    left: &ast::Expr,
    right: &ast::Expr,
    operator: &ast::BinaryOperator,
    schema: RowScope<'_>,
) -> Result<()> {
    let left = extract_row_fields(left);
    let right = extract_row_fields(right);
    if left.len() != right.len() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "subquery has too many columns",
        ));
    }
    for (left, right) in left.iter().zip(right) {
        validate_comparison_type(operator, resolve_operator_type(left, right, schema)?)?;
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn extract_row_fields(expr: &ast::Expr) -> &[ast::Expr] {
    match expr {
        ast::Expr::Tuple(fields) => fields,
        expr => std::slice::from_ref(expr),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_membership(
    expr: &ast::Expr,
    list: &[ast::Expr],
    negated: bool,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    validate_membership_types(expr, list, schema)?;
    let mut result = Value::Bool(false);
    for candidate in list {
        result = evaluate_boolean_operator(
            &ast::BinaryOperator::Or,
            result,
            evaluate_row_comparison(
                expr,
                candidate,
                &ast::BinaryOperator::Eq,
                schema,
                row,
                context,
            )?,
        )?;
        if result == Value::Bool(true) {
            break;
        }
    }
    if negated {
        evaluate_unary_operator(ast::UnaryOperator::Not, result)
    } else {
        Ok(result)
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_quantified(
    left: &ast::Expr,
    compare_op: &ast::BinaryOperator,
    right: &ast::Expr,
    all: bool,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    if let Ok(Some(element_type)) =
        infer_expression_type(right, schema).map(BaseType::get_array_element_type)
    {
        if element_type != BaseType::Uuid
            || !matches!(
                (all, compare_op),
                (false, ast::BinaryOperator::Eq) | (true, ast::BinaryOperator::NotEq)
            )
        {
            return crate::error::reject_unsupported(
                "quantified array comparison is not implemented",
            );
        }
        let left_type = infer_expression_type(left, schema)?;
        let comparison_type =
            coercion::resolve_common_type(left_type, element_type).ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedFunction,
                    "operator does not exist for quantified comparison",
                )
            })?;
        validate_comparison_type(compare_op, comparison_type)?;
        let left = evaluate_and_coerce(
            left,
            comparison_type,
            CastContext::Implicit,
            schema,
            row,
            context,
        )?;
        let array = super::evaluate(right, schema, row, context)?;
        let Value::Array { values, .. } = array else {
            return Ok(Value::Null);
        };
        let mut result = Value::Bool(all);
        for candidate in values {
            let candidate = coercion::coerce(
                candidate,
                element_type,
                crate::value::PgType::create(comparison_type),
                CastContext::Implicit,
                &context.timezone,
            )?;
            let comparison = if left.is_null() || candidate.is_null() {
                Value::Null
            } else {
                evaluate_comparison(compare_op, &left, &candidate)?
            };
            result = evaluate_boolean_operator(
                if all {
                    &ast::BinaryOperator::And
                } else {
                    &ast::BinaryOperator::Or
                },
                result,
                comparison,
            )?;
            if result == Value::Bool(!all) {
                break;
            }
        }
        return Ok(result);
    }
    let candidates = extract_row_fields(right);
    let mut result = Value::Bool(all);
    for candidate in candidates {
        result = evaluate_boolean_operator(
            if all {
                &ast::BinaryOperator::And
            } else {
                &ast::BinaryOperator::Or
            },
            result,
            evaluate_row_comparison(left, candidate, compare_op, schema, row, context)?,
        )?;
        if result == Value::Bool(!all) {
            break;
        }
    }
    Ok(result)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_row_comparison(
    left: &ast::Expr,
    right: &ast::Expr,
    operator: &ast::BinaryOperator,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    let left = extract_row_fields(left);
    let right = extract_row_fields(right);
    if left.len() != right.len() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "subquery has too many columns",
        ));
    }
    let mut result = Value::Bool(true);
    for (left, right) in left.iter().zip(right) {
        let target = resolve_operator_type(left, right, schema)?;
        let left = evaluate_and_coerce(left, target, CastContext::Implicit, schema, row, context)?;
        let right =
            evaluate_and_coerce(right, target, CastContext::Implicit, schema, row, context)?;
        let evaluate_comparison = if left.is_null() || right.is_null() {
            Value::Null
        } else {
            evaluate_comparison(operator, &left, &right)?
        };
        result = evaluate_boolean_operator(&ast::BinaryOperator::And, result, evaluate_comparison)?;
        if result == Value::Bool(false) {
            break;
        }
    }
    Ok(result)
}

#[cfg(test)]
#[path = "comparisons_test.rs"]
mod tests;
